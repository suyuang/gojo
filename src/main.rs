mod eval;
mod search;

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cozy_chess::util::{display_uci_move, parse_uci_move};
use cozy_chess::{Board, Color, File, Move, Piece, Rank, Square};

use crate::eval::{Evaluator, SimpleEvaluator};
use crate::search::{MinimaxSearcher, SearchConfig, SearchEngine, Searcher, StopSignal};

const ENGINE_NAME: &str = "Gojo";
const ENGINE_AUTHOR: &str = "Ian Gui";
<<<<<<< HEAD
const DEFAULT_SEARCH_DEPTH: u32 = 4;
=======
const DEFAULT_MOVETIME_MS: u64 = 1000;
const MOVE_OVERHEAD_MS: u64 = 50;
>>>>>>> 5fff835 (basic alpha-beta pruning and more evaluation metrics)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveMode {
    Normal,
    Infinite,
    Ponder,
}

enum SearchState<S: Searcher + 'static, E: Evaluator + Send + 'static> {
    Idle(SearchEngine<S, E>),
    Searching {
        stop: StopSignal,
        suppress_bestmove: Arc<AtomicBool>,
        mode: ActiveMode,
        handle: JoinHandle<SearchEngine<S, E>>,
    },
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

struct UciEngine<
    S: Searcher + 'static = MinimaxSearcher,
    E: Evaluator + Send + 'static = SimpleEvaluator,
> {
    board: Board,
    history: Vec<u64>,
    debug: bool,
    chess960: bool,
    search_state: Option<SearchState<S, E>>,
}

impl UciEngine<MinimaxSearcher, SimpleEvaluator> {
    fn new() -> Self {
        Self::with_search_engine(SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator))
    }
}

impl<E: Evaluator + Send + 'static> UciEngine<MinimaxSearcher, E> {
    #[cfg(test)]
    fn with_evaluator(evaluator: E) -> Self {
        Self::with_search_engine(SearchEngine::new(MinimaxSearcher::new(), evaluator))
    }
}

impl<S: Searcher + 'static, E: Evaluator + Send + 'static> UciEngine<S, E> {
    fn with_search_engine(search_engine: SearchEngine<S, E>) -> Self {
        let board = Board::default();
        let history = vec![board.hash()];
        Self {
            board,
            history,
            debug: false,
            chess960: false,
            search_state: Some(SearchState::Idle(search_engine)),
        }
    }

    fn run<R: BufRead, W: Write + Send + 'static>(
        &mut self,
        reader: R,
        writer: W,
    ) -> io::Result<()> {
        let shared_out = Arc::new(Mutex::new(writer));

        for line in reader.lines() {
            let line = line?;
            let keep_running = self.handle_command(&line, &shared_out)?;
            if let Ok(mut out) = shared_out.lock() {
                out.flush()?;
            }
            if !keep_running {
                break;
            }
        }

        // On EOF, let a finite search finish cleanly; stop any infinite/ponder search so we don't hang.
        self.finish_search_on_eof();
        if let Ok(mut out) = shared_out.lock() {
            out.flush()?;
        }
        Ok(())
    }

    fn handle_command<W: Write + Send + 'static>(
        &mut self,
        line: &str,
        out: &Arc<Mutex<W>>,
    ) -> io::Result<bool> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(&cmd) = tokens.first() else {
            return Ok(true);
        };

        match cmd {
            "uci" => {
                let mut w = out.lock().unwrap();
                self.handle_uci(&mut *w)?;
            }
            "debug" => self.handle_debug(&tokens[1..]),
            "isready" => {
                let mut w = out.lock().unwrap();
                writeln!(w, "readyok")?;
            }
            "setoption" => {
                let mut w = out.lock().unwrap();
                self.handle_setoption(&tokens[1..], &mut *w)?;
            }
            "ucinewgame" => self.handle_ucinewgame(),
            "position" => {
                self.stop_active_search(true);
                let mut w = out.lock().unwrap();
                self.handle_position(&tokens[1..], &mut *w)?;
            }
            "go" => self.handle_go(&tokens[1..], out)?,
            "stop" => self.stop_active_search(false),
            "ponderhit" => self.handle_ponderhit(),
            "wait" => self.wait_for_normal_search(),
            "quit" => {
                self.stop_active_search(true);
                return Ok(false);
            }
            "d" => {
                let mut w = out.lock().unwrap();
                self.handle_display(&mut *w)?;
            }
            _ => {
                if self.debug {
                    let mut w = out.lock().unwrap();
                    writeln!(w, "info string unknown command: {cmd}")?;
                }
            }
        }

        Ok(true)
    }

    fn stop_active_search(&mut self, suppress_bestmove: bool) {
        match self.search_state.take() {
            Some(SearchState::Searching {
                stop,
                suppress_bestmove: suppress_flag,
                handle,
                ..
            }) => {
                if suppress_bestmove {
                    suppress_flag.store(true, Ordering::Release);
                }
                stop.stop();
                let engine = handle.join().expect("search thread panicked");
                self.search_state = Some(SearchState::Idle(engine));
            }
            other => {
                self.search_state = other;
            }
        }
    }

    fn wait_for_normal_search(&mut self) {
        let is_normal = matches!(
            self.search_state,
            Some(SearchState::Searching {
                mode: ActiveMode::Normal,
                ..
            })
        );
        if is_normal && let Some(SearchState::Searching { handle, .. }) = self.search_state.take() {
            let engine = handle.join().expect("search thread panicked");
            self.search_state = Some(SearchState::Idle(engine));
        }
    }

    fn finish_search_on_eof(&mut self) {
        match self.search_state {
            Some(SearchState::Searching {
                mode: ActiveMode::Normal,
                ..
            }) => self.wait_for_normal_search(),
            Some(SearchState::Searching { .. }) => self.stop_active_search(true),
            _ => {}
        }
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
        self.stop_active_search(true);
        self.board = Board::default();
        self.history.clear();
        self.history.push(self.board.hash());
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
        Ok(())
    }

    fn handle_go<W: Write + Send + 'static>(
        &mut self,
        args: &[&str],
        out: &Arc<Mutex<W>>,
    ) -> io::Result<()> {
        let params = self.parse_go_params(args);

        if let Some(depth) = params.perft {
            self.stop_active_search(true);
            let mut w = out.lock().unwrap();
            return self.run_perft_divide(depth, &mut *w);
        }

        // Stop any previous search before starting a new one.
        self.stop_active_search(true);

        let SearchState::Idle(mut search_engine) = self
            .search_state
            .take()
            .expect("search_state is always populated")
        else {
            unreachable!("stop_active_search guarantees Idle state");
        };

        let mode = if params.infinite {
            ActiveMode::Infinite
        } else if params.ponder {
            ActiveMode::Ponder
        } else {
            ActiveMode::Normal
        };

        let config = SearchConfig {
            max_depth: params.depth,
            max_time: compute_time_limit(&self.board, &params),
            max_nodes: params.nodes,
            searchmoves: params.searchmoves,
        };

        let stop = search_engine.stop_signal();
        // Reset synchronously before spawning so an immediate `stop` command can never race.
        stop.reset();
        let stop_in_thread = stop.clone();
        let suppress_bestmove = Arc::new(AtomicBool::new(false));
        let suppress_in_thread = Arc::clone(&suppress_bestmove);
        let board = self.board.clone();
        let chess960 = self.chess960;
        let writer = Arc::clone(out);
        let wait_for_stop = matches!(mode, ActiveMode::Infinite | ActiveMode::Ponder);

        let handle = thread::spawn(move || {
            let result =
                search_engine.search_with_info_preserving_stop(&board, &config, &mut |info| {
                    if let Some(mv) = info.best_move {
                        let uci_score = match board.side_to_move() {
                            Color::White => info.score,
                            Color::Black => info.score.saturating_neg(),
                        };
                        let mv_str = format_move(&board, chess960, mv);
                        if let Ok(mut w) = writer.lock() {
                            let _ = writeln!(
                                w,
                                "info depth {} score cp {} nodes {} pv {}",
                                info.depth, uci_score, info.nodes, mv_str
                            );
                            let _ = w.flush();
                        }
                    }
                });

            // In `go infinite` or `go ponder`, UCI requires waiting for `stop`/`ponderhit`
            // before sending `bestmove`, even if max depth or forced mate was reached early.
            if wait_for_stop {
                while !stop_in_thread.is_stopped() {
                    thread::sleep(Duration::from_millis(2));
                }
            }

            if !suppress_in_thread.load(Ordering::Acquire)
                && let Ok(mut w) = writer.lock()
            {
                let _ = write_bestmove(&board, chess960, result.best_move, &mut *w);
                let _ = w.flush();
            }

            search_engine
        });

        self.search_state = Some(SearchState::Searching {
            stop,
            suppress_bestmove,
            mode,
            handle,
        });

        Ok(())
    }

    fn handle_ponderhit(&mut self) {
        let is_ponder = matches!(
            self.search_state,
            Some(SearchState::Searching {
                mode: ActiveMode::Ponder,
                ..
            })
        );
        if is_ponder {
            self.stop_active_search(false);
        }
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
}

fn compute_time_limit(board: &Board, params: &GoParams) -> Option<Duration> {
    if params.infinite || params.ponder {
        return None;
    }

    if let Some(movetime) = params.movetime {
        return Some(Duration::from_millis(movetime.max(1)));
    }

    let (time_ms, inc_ms) = match board.side_to_move() {
        Color::White => (params.wtime, params.winc.unwrap_or(0)),
        Color::Black => (params.btime, params.binc.unwrap_or(0)),
    };

    if let Some(time_ms) = time_ms {
        let moves_to_go = params.movestogo.unwrap_or(30).max(1);
        let usable = time_ms.saturating_sub(MOVE_OVERHEAD_MS);
        let base = usable / moves_to_go + inc_ms * 3 / 4;
        let cap = if moves_to_go == 1 { usable } else { usable / 2 };
        let alloc_ms = base.min(cap).max(10);
        return Some(Duration::from_millis(alloc_ms));
    }

    if params.depth.is_some() || params.nodes.is_some() {
        return None;
    }

    Some(Duration::from_millis(DEFAULT_MOVETIME_MS))
}

fn write_bestmove(
    board: &Board,
    chess960: bool,
    bestmove: Option<Move>,
    out: &mut impl Write,
) -> io::Result<()> {
    match bestmove {
        Some(mv) => {
            let mv_str = format_move(board, chess960, mv);
            writeln!(out, "bestmove {mv_str}")
        }
        None => writeln!(out, "bestmove 0000"),
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
    engine.run(stdin.lock(), stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn run_script(script: &str) -> (UciEngine, String) {
        let mut engine = UciEngine::new();
        let buf = SharedBuf::default();
        engine
            .run(Cursor::new(script.as_bytes()), buf.clone())
            .expect("UCI loop should not fail on in-memory IO");
        (engine, buf.contents())
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
             wait\n\
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
             wait\n\
             quit\n",
        );
        assert!(output.lines().any(|l| l == "bestmove e1g1"));
    }

    #[test]
    fn go_returns_legal_move_and_handles_no_legal_moves() {
        let (engine, output) =
            run_script("position startpos\ngo depth 3 movetime 500\nwait\nquit\n");
        let bestmove_line = output
            .lines()
            .find(|l| l.starts_with("bestmove "))
            .expect("expected bestmove line");
        let mv_str = bestmove_line.strip_prefix("bestmove ").unwrap();
        let mv = parse_uci_move(&engine.board, mv_str).expect("valid UCI move");
        assert!(engine.board.is_legal(mv));

        // Fool's mate: White is checkmated, no legal moves.
        let (_, mate_out) =
            run_script("position startpos moves f2f3 e7e5 g2g4 d8h4\ngo\nwait\nquit\n");
        assert_eq!(mate_out.trim(), "bestmove 0000");
    }

    #[test]
    fn go_infinite_and_ponder_wait_for_stop_or_ponderhit_and_allow_isready() {
        let mut engine = UciEngine::new();
        let buf = SharedBuf::default();
        let out = Arc::new(Mutex::new(buf.clone()));

        engine.handle_command("position startpos", &out).unwrap();
        engine.handle_command("go infinite", &out).unwrap();
        thread::sleep(Duration::from_millis(20));

        // `isready` must remain responsive while searching in the background.
        engine.handle_command("isready", &out).unwrap();
        let before_stop = buf.contents();
        assert!(before_stop.lines().any(|l| l == "readyok"));
        assert!(!before_stop.contains("bestmove"));

        engine.handle_command("stop", &out).unwrap();
        let after_stop = buf.contents();
        assert!(after_stop.lines().any(|l| l.starts_with("bestmove ")));

        let ponder_buf = SharedBuf::default();
        let ponder_out = Arc::new(Mutex::new(ponder_buf.clone()));
        engine.handle_command("go ponder", &ponder_out).unwrap();
        thread::sleep(Duration::from_millis(15));
        assert!(!ponder_buf.contents().contains("bestmove"));

        engine.handle_command("ponderhit", &ponder_out).unwrap();
        assert!(
            ponder_buf
                .contents()
                .lines()
                .any(|l| l.starts_with("bestmove "))
        );
    }

    #[test]
    fn go_movetime_iteratively_deepens_past_depth_three() {
        let (_, output) = run_script(
            "position fen 6k1/5p2/8/8/8/8/5P2/4R1K1 w - - 0 1\n\
             go movetime 120\n\
             wait\n\
             quit\n",
        );
        let max_depth = output
            .lines()
            .filter_map(|l| l.strip_prefix("info depth "))
            .filter_map(|rest| rest.split_whitespace().next())
            .filter_map(|d| d.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        assert!(
            max_depth > 3,
            "expected iterative deepening past depth 3, got max depth {max_depth}\n{output}"
        );
        assert!(output.lines().any(|l| l.starts_with("bestmove ")));
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
        let buf = SharedBuf::default();
        engine
            .run(
                Cursor::new(b"position startpos\ngo depth 1\nwait\nquit\n"),
                buf.clone(),
            )
            .unwrap();
        let output = buf.contents();
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
             wait\n\
             quit\n",
        );
        assert!(output.lines().any(|l| l == "bestmove d1d8"));
    }
}
