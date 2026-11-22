use cozy_chess::{BitBoard, Board, Color, File, Piece, Rank, Square};

use crate::eval::{Evaluator, INF, NEG_INF, Score};

const QUEEN_VALUE: Score = 900;
const ROOK_VALUE: Score = 500;
const BISHOP_VALUE: Score = 310;
const KNIGHT_VALUE: Score = 300;
const PAWN_VALUE: Score = 100;

/// Bonus per non-king piece on the opponent's half of the board.
const SPACE_BONUS: Score = 10;

/// King safety weights rewarding proximity to the player's own corner (a1/h1 or a8/h8).
const KING_CORNER_FILE_UNIT: Score = 8;
const KING_CORNER_RANK_UNIT: Score = 8;

/// Centralization score by Chebyshev ring distance from the center (d4/d5/e4/e5):
/// ring 0 = center 4, ring 1 = inner ring, ring 2 = outer ring, ring 3 = board edge.
const CENTER_RING_BONUS: [Score; 4] = [20, 10, 0, -10];

/// Pawn structure weights.
const DOUBLED_PAWN_PENALTY: Score = 20;
const ISOLATED_PAWN_PENALTY: Score = 20;
const PASSED_PAWN_BASE_BONUS: Score = 15;
const PASSED_PAWN_RANK_BONUS: Score = 10;

/// Simple positional and material evaluator based on `src/eval/rule.md`.
///
/// Scores are in centipawns with White positive and Black negative:
/// - Black checkmated: `INF`; White checkmated: `NEG_INF` (`-INF`)
/// - Material: Queen = 900 (9), Rook = 500 (5), Bishop = 310 (3.1), Knight = 300 (3), Pawn = 100 (1)
/// - Space: bonus for non-king pieces on the opponent's half of the board
/// - King safety: bonus for king proximity to its own corner
/// - Centralization: bonus near the center, deduction on the board edges
/// - Pawn structure: penalty for doubled and isolated pawns, bonus for passed pawns
#[derive(Debug, Default, Clone, Copy)]
pub struct SimpleEvaluator;

impl Evaluator for SimpleEvaluator {
    fn evaluate(&self, board: &Board) -> Score {
        if !board.checkers().is_empty() && !board.generate_moves(|_| true) {
            return match board.side_to_move() {
                Color::Black => INF,
                Color::White => NEG_INF,
            };
        }

        material_score(board)
            + space_score(board)
            + king_safety_score(board)
            + centralization_score(board)
            + pawn_structure_score(board)
    }
}

fn material_score(board: &Board) -> Score {
    const PIECES: [(Piece, Score); 5] = [
        (Piece::Queen, QUEEN_VALUE),
        (Piece::Rook, ROOK_VALUE),
        (Piece::Bishop, BISHOP_VALUE),
        (Piece::Knight, KNIGHT_VALUE),
        (Piece::Pawn, PAWN_VALUE),
    ];

    let mut score: Score = 0;
    for (piece, value) in PIECES {
        let white_count = board.colored_pieces(Color::White, piece).len() as Score;
        let black_count = board.colored_pieces(Color::Black, piece).len() as Score;
        score += (white_count - black_count) * value;
    }
    score
}

fn opponent_half(color: Color) -> BitBoard {
    match color {
        Color::White => {
            Rank::Fifth.bitboard()
                | Rank::Sixth.bitboard()
                | Rank::Seventh.bitboard()
                | Rank::Eighth.bitboard()
        }
        Color::Black => {
            Rank::First.bitboard()
                | Rank::Second.bitboard()
                | Rank::Third.bitboard()
                | Rank::Fourth.bitboard()
        }
    }
}

fn space_score(board: &Board) -> Score {
    let kings = board.pieces(Piece::King);
    let white_space =
        (board.colors(Color::White) & !kings & opponent_half(Color::White)).len() as Score;
    let black_space =
        (board.colors(Color::Black) & !kings & opponent_half(Color::Black)).len() as Score;
    (white_space - black_space) * SPACE_BONUS
}

fn king_corner_safety(sq: Square, color: Color) -> Score {
    let file = sq.file() as Score;
    let rank = sq.rank() as Score;
    let file_dist_to_corner = file.min(7 - file);
    let rank_dist_to_corner = match color {
        Color::White => rank,
        Color::Black => 7 - rank,
    };

    (3 - file_dist_to_corner) * KING_CORNER_FILE_UNIT
        + (7 - rank_dist_to_corner) * KING_CORNER_RANK_UNIT
}

fn king_safety_score(board: &Board) -> Score {
    let white_king = board.king(Color::White);
    let black_king = board.king(Color::Black);
    king_corner_safety(white_king, Color::White) - king_corner_safety(black_king, Color::Black)
}

fn square_centralization(sq: Square) -> Score {
    let file = sq.file() as usize;
    let rank = sq.rank() as usize;
    let df = if file < 4 { 3 - file } else { file - 4 };
    let dr = if rank < 4 { 3 - rank } else { rank - 4 };
    CENTER_RING_BONUS[df.max(dr)]
}

fn centralization_score(board: &Board) -> Score {
    let kings = board.pieces(Piece::King);
    let mut score: Score = 0;

    for sq in board.colors(Color::White) & !kings {
        score += square_centralization(sq);
    }
    for sq in board.colors(Color::Black) & !kings {
        score -= square_centralization(sq);
    }

    score
}

fn adjacent_files_mask(file: File) -> BitBoard {
    let idx = file as usize;
    let mut mask = BitBoard::EMPTY;
    if idx > 0 {
        mask |= File::ALL[idx - 1].bitboard();
    }
    if idx + 1 < File::ALL.len() {
        mask |= File::ALL[idx + 1].bitboard();
    }
    mask
}

fn ahead_ranks_mask(color: Color, rank: Rank) -> BitBoard {
    let idx = rank as usize;
    let mut mask = BitBoard::EMPTY;
    match color {
        Color::White => {
            for &r in &Rank::ALL[idx + 1..] {
                mask |= r.bitboard();
            }
        }
        Color::Black => {
            for &r in &Rank::ALL[..idx] {
                mask |= r.bitboard();
            }
        }
    }
    mask
}

fn pawn_structure_for_color(board: &Board, color: Color) -> Score {
    let own_pawns = board.colored_pieces(color, Piece::Pawn);
    let enemy_pawns = board.colored_pieces(!color, Piece::Pawn);
    let mut score: Score = 0;

    // Penalize doubled pawns (each extra pawn on the same file).
    for &file in &File::ALL {
        let count = (own_pawns & file.bitboard()).len() as Score;
        if count > 1 {
            score -= (count - 1) * DOUBLED_PAWN_PENALTY;
        }
    }

    // Penalize isolated pawns and reward passed pawns.
    for sq in own_pawns {
        let file = sq.file();
        let rank = sq.rank();
        let adj_files = adjacent_files_mask(file);

        if (own_pawns & adj_files).is_empty() {
            score -= ISOLATED_PAWN_PENALTY;
        }

        let passed_mask = (file.bitboard() | adj_files) & ahead_ranks_mask(color, rank);
        if (enemy_pawns & passed_mask).is_empty() {
            let rel_rank = match color {
                Color::White => rank as Score,
                Color::Black => 7 - rank as Score,
            };
            score += PASSED_PAWN_BASE_BONUS + rel_rank * PASSED_PAWN_RANK_BONUS;
        }
    }

    score
}

fn pawn_structure_score(board: &Board) -> Score {
    pawn_structure_for_color(board, Color::White) - pawn_structure_for_color(board, Color::Black)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_fen(fen: &str) -> Score {
        let board: Board = fen.parse().expect("valid FEN");
        SimpleEvaluator.evaluate(&board)
    }

    #[test]
    fn startpos_is_equal() {
        assert_eq!(SimpleEvaluator.evaluate(&Board::default()), 0);
    }

    #[test]
    fn black_checkmated_is_positive_infinity() {
        // Scholar's mate: Black to move and checkmated.
        let mut board = Board::default();
        for mv in ["e2e4", "e7e5", "f1c4", "b8c6", "d1h5", "g8f6", "h5f7"] {
            board.play(mv.parse().unwrap());
        }
        assert_eq!(SimpleEvaluator.evaluate(&board), INF);
    }

    #[test]
    fn white_checkmated_is_negative_infinity() {
        // Fool's mate: White to move and checkmated.
        let mut board = Board::default();
        for mv in ["f2f3", "e7e5", "g2g4", "d8h4"] {
            board.play(mv.parse().unwrap());
        }
        assert_eq!(SimpleEvaluator.evaluate(&board), NEG_INF);
    }

    #[test]
    fn checkmate_overrides_material_imbalance() {
        // White has only a rook and king vs Black's queen and rook, yet Black is checkmated on a8.
        assert_eq!(eval_fen("k2R4/8/1K6/8/8/8/8/5qr1 b - - 0 1"), INF);
    }

    #[test]
    fn material_piece_values_match_rules() {
        let material = |fen: &str| material_score(&fen.parse::<Board>().unwrap());

        // White pieces (positive)
        assert_eq!(material("4k3/8/8/8/8/8/8/3QK3 w - - 0 1"), 900);
        assert_eq!(material("4k3/8/8/8/8/8/8/3RK3 w - - 0 1"), 500);
        assert_eq!(material("4k3/8/8/8/8/8/8/3BK3 w - - 0 1"), 310);
        assert_eq!(material("4k3/8/8/8/8/8/8/3NK3 w - - 0 1"), 300);
        assert_eq!(material("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1"), 100);

        // Black pieces (negative)
        assert_eq!(material("3qk3/8/8/8/8/8/8/4K3 w - - 0 1"), -900);
        assert_eq!(material("3rk3/8/8/8/8/8/8/4K3 w - - 0 1"), -500);
        assert_eq!(material("3bk3/8/8/8/8/8/8/4K3 w - - 0 1"), -310);
        assert_eq!(material("3nk3/8/8/8/8/8/8/4K3 w - - 0 1"), -300);
        assert_eq!(material("4k3/4p3/8/8/8/8/8/4K3 w - - 0 1"), -100);
    }

    #[test]
    fn space_rewards_pieces_on_opponent_half() {
        // Same centralization ring (e4 vs e5 are both ring 0), only space differs.
        let own_half = eval_fen("4k3/8/8/8/4N3/8/8/4K3 w - - 0 1");
        let opp_half = eval_fen("4k3/8/8/4N3/8/8/8/4K3 w - - 0 1");
        assert_eq!(opp_half - own_half, SPACE_BONUS);

        // Mirror for Black: knight on e4 (White's half) is better for Black (more negative) than e5.
        let black_own = eval_fen("4k3/8/8/4n3/8/8/8/4K3 w - - 0 1");
        let black_opp = eval_fen("4k3/8/8/8/4n3/8/8/4K3 w - - 0 1");
        assert_eq!(black_opp - black_own, -SPACE_BONUS);
    }

    #[test]
    fn king_safety_rewards_proximity_to_own_corner() {
        // White king: h1 (corner) > g1 (castled) > e1 (center back rank) > e4 (exposed)
        let k_h1 = eval_fen("4k3/8/8/8/8/8/8/7K w - - 0 1");
        let k_g1 = eval_fen("4k3/8/8/8/8/8/8/6K1 w - - 0 1");
        let k_e1 = eval_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1");
        let k_e4 = eval_fen("4k3/8/8/8/4K3/8/8/8 w - - 0 1");
        assert!(k_h1 > k_g1);
        assert!(k_g1 > k_e1);
        assert!(k_e1 > k_e4);

        // Mirror for Black: h8 (corner) is better for Black (more negative) than e8 or e5.
        let bk_h8 = eval_fen("7k/8/8/8/8/8/8/4K3 w - - 0 1");
        let bk_e8 = eval_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1");
        let bk_e5 = eval_fen("8/8/8/4k3/8/8/8/4K3 w - - 0 1");
        assert!(bk_h8 < bk_e8);
        assert!(bk_e8 < bk_e5);
    }

    #[test]
    fn centralization_rewards_center_and_penalizes_edges() {
        // All on White's half so space is identical: d4 (ring 0) > c3 (ring 1) > b2 (ring 2) > a2/a1 (ring 3 edge)
        let center = eval_fen("4k3/8/8/8/3N4/8/8/4K3 w - - 0 1");
        let inner = eval_fen("4k3/8/8/8/8/2N5/8/4K3 w - - 0 1");
        let outer = eval_fen("4k3/8/8/8/8/8/1N6/4K3 w - - 0 1");
        let edge = eval_fen("4k3/8/8/8/8/8/N7/4K3 w - - 0 1");
        assert!(center > inner);
        assert!(inner > outer);
        assert!(outer > edge);
        // Edge knight gets a negative centralization deduction relative to pure material (300).
        assert_eq!(edge, KNIGHT_VALUE + CENTER_RING_BONUS[3]);
    }

    #[test]
    fn pawn_structure_penalizes_doubled_and_isolated_and_rewards_passed() {
        // Doubled pawns vs side-by-side pawns (blocking Black pawns so none are passed).
        let connected: Board = "4k3/3pp3/8/8/8/8/3PP3/4K3 w - - 0 1".parse().unwrap();
        let doubled: Board = "4k3/3pp3/8/8/8/4P3/4P3/4K3 w - - 0 1".parse().unwrap();
        assert!(pawn_structure_score(&doubled) < pawn_structure_score(&connected));

        // Isolated pawn (e2 with no d/f friendly pawns) vs supported pair (d2, e2) — blocked by Black pawns.
        let isolated_white: Board = "4k3/3ppp2/8/8/8/8/4P3/4K3 w - - 0 1".parse().unwrap();
        assert_eq!(
            pawn_structure_for_color(&isolated_white, Color::White),
            -ISOLATED_PAWN_PENALTY
        );

        // Passed pawn bonus: White pawn on e4 with no Black pawns on d/e/f ahead of it vs blocked by Black pawn on e6.
        let passed: Board = "4k3/p7/8/8/4P3/8/8/4K3 w - - 0 1".parse().unwrap();
        let blocked: Board = "4k3/p7/4p3/8/4P3/8/8/4K3 w - - 0 1".parse().unwrap();
        assert!(
            pawn_structure_for_color(&passed, Color::White)
                > pawn_structure_for_color(&blocked, Color::White)
        );

        // More advanced passed pawn receives a larger bonus.
        let passed_rank4: Board = "4k3/8/8/8/4P3/8/8/4K3 w - - 0 1".parse().unwrap();
        let passed_rank6: Board = "4k3/8/4P3/8/8/8/8/4K3 w - - 0 1".parse().unwrap();
        assert!(
            pawn_structure_for_color(&passed_rank6, Color::White)
                > pawn_structure_for_color(&passed_rank4, Color::White)
        );
    }

    #[test]
    fn turn_invariance_and_color_symmetry() {
        let white_to_move = "3rk2n/4p3/8/8/8/8/3PP3/2BQK3 w - - 0 1";
        let black_to_move = "3rk2n/4p3/8/8/8/8/3PP3/2BQK3 b - - 0 1";
        assert_eq!(eval_fen(white_to_move), eval_fen(black_to_move));

        // Exact vertical mirror with swapped colors must negate the evaluation.
        let pos_w = "4k3/2p5/8/3N4/8/2P5/5P2/6K1 w - - 0 1";
        let pos_b = "6k1/5p2/2p5/8/3n4/8/2P5/4K3 w - - 0 1";
        assert_eq!(eval_fen(pos_w), -eval_fen(pos_b));
    }
}
