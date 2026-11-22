mod eval;
mod search;

use std::io::{self, BufRead, Write};

use cozy_chess::util::{display_uci_move, parse_uci_move};
use cozy_chess::{Board, Color, File, Move, Piece, Rank, Square};

use crate::eval::{Evaluator, SimpleEvaluator};
use crate::search::{MinimaxSearcher, SearchConfig, SearchEngine, SearchResult, Searcher};

const ENGINE_NAME: &str = "Gojo";
const ENGINE_AUTHOR: &str = "Ian Gui";
const DEFAULT_SEARCH_DEPTH: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Infinite,
    Ponder,
}

#[derive(Debug, Clone)]
struct PendingSearch {
    mode: SearchMode,
    bestmove: Option<Move>,
    board: Board,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct GoParams {
    searchmoves: Vec<Move>,
    ponder: bool,
    wtime: Option<u64>,
    btime: Option<u64>,
    winc: Option<u64>,
    binc: Option<u64>,
    movestogo: Option<u64>,
    depth: Option<u32>,
    nodes: Option<u64>,
    mate: Option<u32>,
    movetime: Option<u64>,
    infinite: bool,
    perft: Option<u32>,
}

struct UciEngine<S: Searcher = MinimaxSearcher, E: Evaluator = SimpleEvaluator> {
    board: Board,
    history: Vec<u64>,
    debug: bool,
    chess960: bool,
    pending_search: Option<PendingSearch>,
    search_engine: SearchEngine<S, E>,
}

impl UciEngine<MinimaxSearcher, SimpleEvaluator> {
    fn new() -> Self {
        Self::with_search_engine(SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator))
    }
}

impl<E: Evaluator> UciEngine<MinimaxSearcher, E> {
    #[cfg(test)]
    fn with_evaluator(evaluator: E) -> Self {
        Self::with_search_engine(SearchEngine::new(MinimaxSearcher::new(), evaluator))
    }
}

impl<S: Searcher, E: Evaluator> UciEngine<S, E> {
    fn with_search_engine(search_engine: SearchEngine<S, E>) -> Self {
        let board = Board::default();
        let history = vec![board.hash()];
        Self {
            board,
            history,
            debug: false,
            chess960: false,
            pending_search: None,
            search_engine,
        }
    }

    fn run(&mut self, reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
        for line in reader.lines() {
            let line = line?;
            let keep_running = self.handle_command(&line, &mut writer)?;
            writer.flush()?;
            if !keep_running {
                break;
            }
        }
        Ok(())
    }

    fn handle_command(&mut self, line: &str, out: &mut impl Write) -> io::Result<bool> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&cmd) = tokens.first() else {
            return Ok(true);
        };

        match cmd {
            "uci" => self.handle_uci(out)?,
            "debug" => self.handle_debug(&tokens[1..]),
            "isready" => writeln!(out, "readyok")?,
            "setoption" => self.handle_setoption(&tokens[1..], out)?,
            "ucinewgame" => self.handle_ucinewgame(),
            "position" => self.handle_position(&tokens[1..], out)?,
            "go" => self.handle_go(&tokens[1..], out)?,
            "stop" => self.handle_stop(out)?,
            "ponderhit" => self.handle_ponderhit(out)?,
            "quit" => return Ok(false),
            "d" => self.handle_display(out)?,
            _ => {
                if self.debug {
                    writeln!(out, "info string unknown command: {cmd}")?;
                }
            }
        }

        Ok(true)
    }

    fn handle_uci(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(out, "id name {ENGINE_NAME}")?;
        writeln!(out, "id author {ENGINE_AUTHOR}")?;
        writeln!(out, "option name UCI_Chess960 type check default false")?;
        writeln!(out, "uciok")?;
        Ok(())
    }

    fn handle_debug(&mut self, args: &[&str]) {
        match args.first().copied() {
            Some("on") => self.debug = true,
            Some("off") => self.debug = false,
            _ => {}
        }
    }

    fn handle_setoption(&mut self, args: &[&str], out: &mut impl Write) -> io::Result<()> {
        let Some(name_idx) = args.iter().position(|&t| t == "name") else {
            return Ok(());
        };
        let value_idx = args[name_idx + 1..]
            .iter()
            .position(|&t| t == "value")
            .map(|rel| name_idx + 1 + rel);

        let name_end = value_idx.unwrap_or(args.len());
        let name = args[name_idx + 1..name_end].join(" ");
        let value = value_idx.map(|idx| args[idx + 1..].join(" "));

        if name.eq_ignore_ascii_case("UCI_Chess960") {
            if let Some(val) = value {
                if val.eq_ignore_ascii_case("true") {
                    self.chess960 = true;
                } else if val.eq_ignore_ascii_case("false") {
                    self.chess960 = false;
                } else if self.debug {
                    writeln!(out, "info string invalid value for UCI_Chess960: {val}")?;
                }
            }
        } else if self.debug {
            writeln!(out, "info string unknown option: {name}")?;
        }

        Ok(())
    }

    fn handle_ucinewgame(&mut self) {
        self.board = Board::default();
        self.history.clear();
        self.history.push(self.board.hash());
        self.pending_search = None;
    }

    fn handle_position(&mut self, args: &[&str], out: &mut impl Write) -> io::Result<()> {
        let Some(&first) = args.first() else {
            return Ok(());
        };

        let moves_idx = args.iter().position(|&t| t == "moves");
        let base_end = moves_idx.unwrap_or(args.len());

        let new_board = match first {
            "startpos" => Ok(Board::default()),
            "fen" => {
                let fen_tokens = &args[1..base_end];
                parse_fen_tokens(fen_tokens)
            }
            _ => Err(format!("invalid position specification: {first}")),
        };

        let mut board = match new_board {
            Ok(b) => b,
            Err(err) => {
                if self.debug {
                    writeln!(out, "info string {err}")?;
                }
                return Ok(());
            }
        };

        let mut history = vec![board.hash()];

        if let Some(idx) = moves_idx {
            for &mv_str in &args[idx + 1..] {
                let parsed = parse_move_for_board(&board, self.chess960, mv_str);
                match parsed {
                    Some(mv) if board.try_play(mv).is_ok() => {
                        history.push(board.hash());
                    }
                    _ => {
                        if self.debug {
                            writeln!(out, "info string illegal or invalid move: {mv_str}")?;
                        }
                        break;
                    }
                }
            }
        }

        self.board = board;
        self.history = history;
        self.pending_search = None;
        Ok(())
    }

    fn handle_go(&mut self, args: &[&str], out: &mut impl Write) -> io::Result<()> {
        let params = self.parse_go_params(args);

        if let Some(depth) = params.perft {
            return self.run_perft_divide(depth, out);
        }

        let result = self.search(&params);
        let bestmove = result.best_move;

        if let Some(mv) = bestmove {
            let uci_score = match self.board.side_to_move() {
                Color::White => result.score,
                Color::Black => result.score.saturating_neg(),
            };
            let mv_str = format_move(&self.board, self.chess960, mv);
            writeln!(
                out,
                "info depth {} score cp {} nodes {} pv {}",
                result.depth, uci_score, result.nodes, mv_str
            )?;
        }

        if params.infinite {
            self.pending_search = Some(PendingSearch {
                mode: SearchMode::Infinite,
                bestmove,
                board: self.board.clone(),
            });
            return Ok(());
        }

        if params.ponder {
            self.pending_search = Some(PendingSearch {
                mode: SearchMode::Ponder,
                bestmove,
                board: self.board.clone(),
            });
            return Ok(());
        }

        self.write_bestmove(&self.board, bestmove, out)
    }

    fn handle_stop(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.search_engine.stop();
        if let Some(pending) = self.pending_search.take() {
            self.write_bestmove(&pending.board, pending.bestmove, out)?;
        }
        Ok(())
    }

    fn handle_ponderhit(&mut self, out: &mut impl Write) -> io::Result<()> {
        if matches!(
            self.pending_search,
            Some(PendingSearch {
                mode: SearchMode::Ponder,
                ..
            })
        ) && let Some(pending) = self.pending_search.take()
        {
            self.write_bestmove(&pending.board, pending.bestmove, out)?;
        }
        Ok(())
    }

    fn handle_display(&self, out: &mut impl Write) -> io::Result<()> {
        for &rank in Rank::ALL.iter().rev() {
            write!(out, "{} ", rank as u8 + 1)?;
            for &file in &File::ALL {
                let sq = Square::new(file, rank);
                let ch = match (self.board.piece_on(sq), self.board.color_on(sq)) {
                    (Some(piece), Some(color)) => piece_to_char(piece, color),
                    _ => '.',
                };
                write!(out, "{ch} ")?;
            }
            writeln!(out)?;
        }
        writeln!(out, "  a b c d e f g h")?;
        writeln!(out, "FEN: {}", self.board)?;
        writeln!(out, "Hash: {:016x}", self.board.hash())?;
        Ok(())
    }

    fn parse_go_params(&self, args: &[&str]) -> GoParams {
        let mut params = GoParams::default();
        let mut i = 0;

        while i < args.len() {
            match args[i] {
                "searchmoves" => {
                    i += 1;
                    while i < args.len() && !is_go_keyword(args[i]) {
                        if let Some(mv) = parse_move_for_board(&self.board, self.chess960, args[i])
                            && self.board.is_legal(mv)
                        {
                            params.searchmoves.push(mv);
                        }
                        i += 1;
                    }
                    continue;
                }
                "ponder" => params.ponder = true,
                "infinite" => params.infinite = true,
                "wtime" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.wtime = Some(v);
                        i += 1;
                    }
                }
                "btime" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.btime = Some(v);
                        i += 1;
                    }
                }
                "winc" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.winc = Some(v);
                        i += 1;
                    }
                }
                "binc" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.binc = Some(v);
                        i += 1;
                    }
                }
                "movestogo" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.movestogo = Some(v);
                        i += 1;
                    }
                }
                "depth" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.depth = Some(v);
                        i += 1;
                    }
                }
                "nodes" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.nodes = Some(v);
                        i += 1;
                    }
                }
                "mate" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.mate = Some(v);
                        i += 1;
                    }
                }
                "movetime" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.movetime = Some(v);
                        i += 1;
                    }
                }
                "perft" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        params.perft = Some(v);
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }

        params
    }

    fn search(&mut self, params: &GoParams) -> SearchResult {
        let config = SearchConfig {
            depth: params.depth.unwrap_or(DEFAULT_SEARCH_DEPTH),
            searchmoves: params.searchmoves.clone(),
        };
        self.search_engine.search_with_config(&self.board, &config)
    }

    fn run_perft_divide(&self, depth: u32, out: &mut impl Write) -> io::Result<()> {
        if depth == 0 {
            writeln!(out, "Nodes searched: 1")?;
            return Ok(());
        }

        let mut root_moves = Vec::new();
        self.board.generate_moves(|moves| {
            root_moves.extend(moves);
            false
        });

        let mut total = 0u64;
        for mv in root_moves {
            let mut child = self.board.clone();
            child.play_unchecked(mv);
            let nodes = perft(&child, depth - 1);
            total += nodes;
            let mv_str = format_move(&self.board, self.chess960, mv);
            writeln!(out, "{mv_str}: {nodes}")?;
        }
        writeln!(out)?;
        writeln!(out, "Nodes searched: {total}")?;
        Ok(())
    }

    fn write_bestmove(
        &self,
        board: &Board,
        bestmove: Option<Move>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        match bestmove {
            Some(mv) => {
                let mv_str = format_move(board, self.chess960, mv);
                writeln!(out, "bestmove {mv_str}")
            }
            None => writeln!(out, "bestmove 0000"),
        }
    }
}

fn is_go_keyword(token: &str) -> bool {
    matches!(
        token,
        "searchmoves"
            | "ponder"
            | "wtime"
            | "btime"
            | "winc"
            | "binc"
            | "movestogo"
            | "depth"
            | "nodes"
            | "mate"
            | "movetime"
            | "infinite"
            | "perft"
    )
}

fn parse_fen_tokens(tokens: &[&str]) -> Result<Board, String> {
    if tokens.is_empty() {
        return Err("missing FEN string".to_string());
    }
    let fen = tokens.join(" ");
    if let Ok(board) = fen.parse::<Board>() {
        return Ok(board);
    }
    if tokens.len() == 4 {
        let extended = format!("{fen} 0 1");
        if let Ok(board) = extended.parse::<Board>() {
            return Ok(board);
        }
    }
    Err(format!("invalid FEN: {fen}"))
}

fn parse_move_for_board(board: &Board, chess960: bool, mv_str: &str) -> Option<Move> {
    if chess960 {
        mv_str.parse::<Move>().ok()
    } else {
        parse_uci_move(board, mv_str).ok()
    }
}

fn format_move(board: &Board, chess960: bool, mv: Move) -> String {
    if chess960 {
        mv.to_string()
    } else {
        display_uci_move(board, mv).to_string()
    }
}

fn piece_to_char(piece: Piece, color: Color) -> char {
    let ch = match piece {
        Piece::Pawn => 'p',
        Piece::Knight => 'n',
        Piece::Bishop => 'b',
        Piece::Rook => 'r',
        Piece::Queen => 'q',
        Piece::King => 'k',
    };
    match color {
        Color::White => ch.to_ascii_uppercase(),
        Color::Black => ch,
    }
}

fn perft(board: &Board, depth: u32) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut nodes = 0u64;
    if depth == 1 {
        board.generate_moves(|moves| {
            nodes += moves.len() as u64;
            false
        });
        return nodes;
    }

    board.generate_moves(|moves| {
        for mv in moves {
            let mut child = board.clone();
            child.play_unchecked(mv);
            nodes += perft(&child, depth - 1);
        }
        false
    });
    nodes
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut engine = UciEngine::new();
    engine.run(stdin.lock(), stdout.lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run_script(script: &str) -> (UciEngine, String) {
        let mut engine = UciEngine::new();
        let mut out = Vec::new();
        engine
            .run(Cursor::new(script.as_bytes()), &mut out)
            .expect("UCI loop should not fail on in-memory IO");
        (engine, String::from_utf8(out).unwrap())
    }

    #[test]
    fn uci_handshake_and_isready() {
        let (_, output) = run_script("uci\nisready\nquit\n");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines,
            vec![
                "id name Gojo",
                "id author Ian Gui",
                "option name UCI_Chess960 type check default false",
                "uciok",
                "readyok",
            ]
        );
    }

    #[test]
    fn position_startpos_and_standard_castling() {
        let (engine, output) = run_script(
            "position startpos moves e2e4 e7e5 g1f3 b8c6 f1c4 f8c5 e1g1\nisready\nquit\n",
        );
        assert_eq!(output.trim(), "readyok");
        assert_eq!(engine.board.piece_on(Square::G1), Some(Piece::King));
        assert_eq!(engine.board.piece_on(Square::F1), Some(Piece::Rook));
        assert_eq!(engine.board.side_to_move(), Color::Black);
        assert_eq!(engine.history.len(), 8);
    }

    #[test]
    fn position_fen_four_and_six_fields() {
        let (engine_full, _) = run_script(
            "position fen rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1 moves e7e5\nquit\n",
        );
        assert_eq!(engine_full.board.piece_on(Square::E5), Some(Piece::Pawn));
        assert_eq!(engine_full.board.side_to_move(), Color::White);

        let (engine_short, _) = run_script(
            "position fen rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq -\nquit\n",
        );
        assert_eq!(engine_short.board.piece_on(Square::E4), Some(Piece::Pawn));
        assert_eq!(engine_short.board.side_to_move(), Color::Black);
    }

    #[test]
    fn chess960_option_parses_and_displays_king_captures_rook() {
        let (engine, output) = run_script(
            "setoption name UCI_Chess960 value true\n\
             position fen rnbqkbnr/ppp2ppp/4p3/3p4/3P4/5NP1/PPP1PPBP/RNBQK2R w KQkq - 0 4\n\
             go searchmoves e1h1\n\
             quit\n",
        );
        assert!(engine.chess960);
        assert!(output.lines().any(|l| l == "bestmove e1h1"));
    }

    #[test]
    fn standard_uci_displays_standard_castling_move() {
        let (_, output) = run_script(
            "position fen rnbqkbnr/ppp2ppp/4p3/3p4/3P4/5NP1/PPP1PPBP/RNBQK2R w KQkq - 0 4\n\
             go searchmoves e1g1\n\
             quit\n",
        );
        assert!(output.lines().any(|l| l == "bestmove e1g1"));
    }

    #[test]
    fn go_returns_legal_move_and_handles_no_legal_moves() {
        let (engine, output) = run_script("position startpos\ngo depth 3 movetime 100\nquit\n");
        let bestmove_line = output
            .lines()
            .find(|l| l.starts_with("bestmove "))
            .expect("expected bestmove line");
        let mv_str = bestmove_line.strip_prefix("bestmove ").unwrap();
        let mv = parse_uci_move(&engine.board, mv_str).expect("valid UCI move");
        assert!(engine.board.is_legal(mv));

        // Fool's mate: White is checkmated, no legal moves.
        let (_, mate_out) = run_script("position startpos moves f2f3 e7e5 g2g4 d8h4\ngo\nquit\n");
        assert_eq!(mate_out.trim(), "bestmove 0000");
    }

    #[test]
    fn go_infinite_and_ponder_wait_for_stop_or_ponderhit() {
        let mut engine = UciEngine::new();
        let mut out = Vec::new();

        engine
            .handle_command("position startpos", &mut out)
            .unwrap();
        engine.handle_command("go infinite", &mut out).unwrap();
        let before_stop = String::from_utf8(out.clone()).unwrap();
        assert!(!before_stop.contains("bestmove"));

        engine.handle_command("stop", &mut out).unwrap();
        let after_stop = String::from_utf8(out).unwrap();
        assert!(after_stop.lines().any(|l| l.starts_with("bestmove ")));

        let mut ponder_out = Vec::new();
        engine.handle_command("go ponder", &mut ponder_out).unwrap();
        assert!(
            !String::from_utf8(ponder_out.clone())
                .unwrap()
                .contains("bestmove")
        );
        engine.handle_command("ponderhit", &mut ponder_out).unwrap();
        assert!(
            String::from_utf8(ponder_out)
                .unwrap()
                .lines()
                .any(|l| l.starts_with("bestmove "))
        );
    }

    #[test]
    fn perft_startpos_counts_match_known_values() {
        let (_, out) = run_script("position startpos\ngo perft 3\nquit\n");
        assert!(out.contains("Nodes searched: 8902"));
    }

    #[test]
    fn ucinewgame_resets_board_and_quit_stops_processing() {
        let (engine, out) = run_script(
            "position startpos moves e2e4 e7e5\n\
             ucinewgame\n\
             quit\n\
             isready\n",
        );
        assert_eq!(engine.board, Board::default());
        assert_eq!(engine.history.len(), 1);
        assert!(out.is_empty());
    }

    #[test]
    fn resilient_to_invalid_inputs_and_unknown_commands() {
        let (engine, out) = run_script(
            "\n   \n\
             unknowncmd foo bar\n\
             position fen not-a-valid-fen\n\
             position startpos moves e2e4 z9z9 e7e5\n\
             isready\n\
             quit\n",
        );
        assert_eq!(engine.board.piece_on(Square::E4), Some(Piece::Pawn));
        assert_eq!(engine.board.piece_on(Square::E5), None);
        assert_eq!(out.trim(), "readyok");
    }

    #[test]
    fn parse_go_params_all_fields() {
        let engine = UciEngine::new();
        let params = engine.parse_go_params(&[
            "searchmoves",
            "e2e4",
            "d2d4",
            "wtime",
            "60000",
            "btime",
            "55000",
            "winc",
            "1000",
            "binc",
            "500",
            "movestogo",
            "30",
            "depth",
            "6",
            "nodes",
            "100000",
            "mate",
            "3",
            "movetime",
            "2500",
            "ponder",
            "infinite",
        ]);
        assert_eq!(params.searchmoves.len(), 2);
        assert_eq!(params.wtime, Some(60000));
        assert_eq!(params.btime, Some(55000));
        assert_eq!(params.winc, Some(1000));
        assert_eq!(params.binc, Some(500));
        assert_eq!(params.movestogo, Some(30));
        assert_eq!(params.depth, Some(6));
        assert_eq!(params.nodes, Some(100000));
        assert_eq!(params.mate, Some(3));
        assert_eq!(params.movetime, Some(2500));
        assert!(params.ponder);
        assert!(params.infinite);
    }

    #[test]
    fn custom_evaluator_score_is_reported_in_go_info() {
        let mut engine = UciEngine::with_evaluator(|b: &Board| b.occupied().len() as eval::Score);
        let mut out = Vec::new();
        engine
            .run(
                Cursor::new(b"position startpos\ngo depth 1\nquit\n"),
                &mut out,
            )
            .unwrap();
        let output = String::from_utf8(out).unwrap();
        assert!(
            output
                .lines()
                .any(|l| l.starts_with("info depth 1 score cp 32 nodes "))
        );
    }

    #[test]
    fn uci_go_finds_mate_in_one_with_minimax() {
        let (_, output) = run_script(
            "position fen 6k1/5ppp/8/8/8/8/5PPP/3R2K1 w - - 0 1\n\
             go depth 1\n\
             quit\n",
        );
        assert!(output.lines().any(|l| l == "bestmove d1d8"));
    }
}
