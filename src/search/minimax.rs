use std::time::Instant;

use cozy_chess::{Board, Color, Move};

use crate::eval::{Evaluator, INF, NEG_INF, Score};
use crate::search::{SearchConfig, SearchResult, Searcher, StopSignal};

const MAX_ITERATIVE_DEPTH: u32 = 64;

struct SearchLimits<'a> {
    stop: &'a StopSignal,
    deadline: Option<Instant>,
    max_nodes: Option<u64>,
}

impl SearchLimits<'_> {
    fn should_stop(&self, nodes: u64) -> bool {
        if self.stop.is_stopped() {
            return true;
        }
        if let Some(limit) = self.max_nodes
            && nodes >= limit
        {
            return true;
        }
        if let Some(deadline) = self.deadline
            && (nodes & 2047) == 0
            && Instant::now() >= deadline
        {
            return true;
        }
        false
    }
}

/// Iterative-deepening minimax search with alpha-beta pruning.
///
/// White maximizes the evaluation score and Black minimizes it, matching the
/// White-positive / Black-negative score convention of [`Evaluator`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MinimaxSearcher;

impl MinimaxSearcher {
    pub fn new() -> Self {
        Self
    }

    fn alphabeta(
        board: &Board,
        depth: u32,
        mut alpha: Score,
        mut beta: Score,
        evaluator: &dyn Evaluator,
        limits: &SearchLimits<'_>,
        nodes: &mut u64,
    ) -> Option<Score> {
        if limits.should_stop(*nodes) {
            return None;
        }

        *nodes += 1;

        if depth == 0 {
            return Some(evaluator.evaluate(board));
        }

        let maximizing = board.side_to_move() == Color::White;
        let mut best_score: Option<Score> = None;
        let mut aborted = false;

        board.generate_moves(|piece_moves| {
            for mv in piece_moves {
                let mut child = board.clone();
                child.play_unchecked(mv);
                let Some(score) =
                    Self::alphabeta(&child, depth - 1, alpha, beta, evaluator, limits, nodes)
                else {
                    aborted = true;
                    return true;
                };

                if maximizing {
                    best_score = Some(match best_score {
                        None => score,
                        Some(cur) => cur.max(score),
                    });
                    alpha = alpha.max(score);
                } else {
                    best_score = Some(match best_score {
                        None => score,
                        Some(cur) => cur.min(score),
                    });
                    beta = beta.min(score);
                }

                if beta <= alpha {
                    return true;
                }
            }
            false
        });

        if aborted {
            return None;
        }

        match best_score {
            Some(score) => Some(score),
            None => {
                if board.checkers().is_empty() {
                    Some(0)
                } else {
                    Some(evaluator.evaluate(board))
                }
            }
        }
    }

    fn search_fixed_depth(
        board: &Board,
        root_moves: &[Move],
        depth: u32,
        evaluator: &dyn Evaluator,
        limits: &SearchLimits<'_>,
        nodes: &mut u64,
    ) -> (Option<Move>, Option<Score>, bool) {
        let maximizing = board.side_to_move() == Color::White;
        let mut alpha = Score::MIN;
        let mut beta = Score::MAX;
        let mut best_move: Option<Move> = None;
        let mut best_score: Option<Score> = None;
        let mut stopped = false;

        for &mv in root_moves {
            if limits.should_stop(*nodes) {
                stopped = true;
                break;
            }

            let mut child = board.clone();
            child.play_unchecked(mv);

            let Some(score) =
                Self::alphabeta(&child, depth - 1, alpha, beta, evaluator, limits, nodes)
            else {
                stopped = true;
                break;
            };

            let is_better = match best_score {
                None => true,
                Some(cur) if maximizing => score > cur,
                Some(cur) => score < cur,
            };

            if is_better {
                best_score = Some(score);
                best_move = Some(mv);
            }

            if maximizing {
                alpha = alpha.max(score);
            } else {
                beta = beta.min(score);
            }

            if beta <= alpha {
                break;
            }
        }

        (best_move, best_score, stopped)
    }
}

impl Searcher for MinimaxSearcher {
    fn search_with_info(
        &mut self,
        board: &Board,
        evaluator: &dyn Evaluator,
        config: &SearchConfig,
        stop: &StopSignal,
        on_info: &mut dyn FnMut(&SearchResult),
    ) -> SearchResult {
        if stop.is_stopped() {
            return SearchResult {
                best_move: None,
                score: 0,
                depth: config.max_depth.unwrap_or(0),
                nodes: 0,
                stopped: true,
            };
        }

        let mut total_nodes = 1u64;

        if config.max_depth == Some(0) {
            let result = SearchResult {
                best_move: None,
                score: evaluator.evaluate(board),
                depth: 0,
                nodes: total_nodes,
                stopped: false,
            };
            on_info(&result);
            return result;
        }

        let mut root_moves: Vec<Move> = if !config.searchmoves.is_empty() {
            config
                .searchmoves
                .iter()
                .copied()
                .filter(|&mv| board.is_legal(mv))
                .collect()
        } else {
            let mut moves = Vec::new();
            board.generate_moves(|piece_moves| {
                moves.extend(piece_moves);
                false
            });
            moves
        };

        if root_moves.is_empty() {
            let score = if board.checkers().is_empty() {
                0
            } else {
                evaluator.evaluate(board)
            };
            return SearchResult {
                best_move: None,
                score,
                depth: config.max_depth.unwrap_or(0),
                nodes: total_nodes,
                stopped: false,
            };
        }

        let limits = SearchLimits {
            stop,
            deadline: config.max_time.map(|d| Instant::now() + d),
            max_nodes: config.max_nodes,
        };
        let max_depth = config.max_depth.unwrap_or(MAX_ITERATIVE_DEPTH);
        let mut last_completed: Option<SearchResult> = None;

        for depth in 1..=max_depth {
            if let Some(prev_best) = last_completed.as_ref().and_then(|r| r.best_move)
                && let Some(pos) = root_moves.iter().position(|&m| m == prev_best)
            {
                root_moves.swap(0, pos);
            }

            let (best_move, best_score, stopped) = Self::search_fixed_depth(
                board,
                &root_moves,
                depth,
                evaluator,
                &limits,
                &mut total_nodes,
            );

            if stopped {
                return match last_completed {
                    Some(mut prev) => {
                        prev.nodes = total_nodes;
                        prev.stopped = true;
                        prev
                    }
                    None => SearchResult {
                        best_move,
                        score: best_score.unwrap_or(0),
                        depth,
                        nodes: total_nodes,
                        stopped: true,
                    },
                };
            }

            let iter_result = SearchResult {
                best_move,
                score: best_score.unwrap_or(0),
                depth,
                nodes: total_nodes,
                stopped: false,
            };
            on_info(&iter_result);

            let is_winning_mate = match board.side_to_move() {
                Color::White => iter_result.score == INF,
                Color::Black => iter_result.score == NEG_INF,
            };
            last_completed = Some(iter_result);

            // Stop deepening early on a single forced move (when not given an explicit depth)
            // or once a forced checkmate for the side to move has been found.
            if config.max_depth.is_none()
                && ((root_moves.len() == 1 && config.max_time.is_some()) || is_winning_mate)
            {
                break;
            }
        }

        last_completed.unwrap_or(SearchResult {
            best_move: None,
            score: 0,
            depth: 0,
            nodes: total_nodes,
            stopped: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::eval::{INF, NEG_INF, SimpleEvaluator};
    use crate::search::SearchEngine;

    #[test]
    fn finds_mate_in_one_for_white() {
        // White rook on d1 can mate Black king on g8 with d1d8#.
        let board: Board = "6k1/5ppp/8/8/8/8/5PPP/3R2K1 w - - 0 1".parse().unwrap();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);

        let result = engine.search(&board, 1);
        assert_eq!(result.best_move, Some("d1d8".parse().unwrap()));
        assert_eq!(result.score, INF);
        assert!(!result.stopped);
    }

    #[test]
    fn finds_mate_in_one_for_black() {
        // Black rook on d8 can mate White king on g1 with d8d1#.
        let board: Board = "3r2k1/5ppp/8/8/8/8/5PPP/6K1 b - - 0 1".parse().unwrap();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);

        let result = engine.search(&board, 1);
        assert_eq!(result.best_move, Some("d8d1".parse().unwrap()));
        assert_eq!(result.score, NEG_INF);
        assert!(!result.stopped);
    }

    #[test]
    fn captures_hanging_queen_at_depth_two() {
        // White rook on e1 can capture an undefended Black queen on e5.
        let board: Board = "6k1/5ppp/8/4q3/8/8/5PPP/4R1K1 w - - 0 1".parse().unwrap();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);

        let result = engine.search(&board, 2);
        assert_eq!(result.best_move, Some("e1e5".parse().unwrap()));
        assert!(result.score > 400);
        assert!(!result.stopped);
    }

    #[test]
    fn avoids_blundering_material_for_black() {
        // Black rook on a8 can capture an undefended White queen on a2; White has no recapture.
        let board: Board = "r5k1/5ppp/8/8/8/8/Q4PPP/6K1 b - - 0 1".parse().unwrap();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);

        let result = engine.search(&board, 2);
        assert_eq!(result.best_move, Some("a8a2".parse().unwrap()));
        assert!(result.score < -400);
    }

    #[test]
    fn avoids_stalemate_when_winning() {
        // White king on f7, queen on e6 vs Black king on h8.
        // e6g6 stalemates Black (score 0), while e6e5 keeps White's winning queen advantage.
        let board: Board = "7k/5K2/4Q3/8/8/8/8/8 w - - 0 1".parse().unwrap();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);
        let config = SearchConfig {
            max_depth: Some(2),
            searchmoves: vec!["e6g6".parse().unwrap(), "e6e5".parse().unwrap()],
            ..SearchConfig::default()
        };

        let result = engine.search_with_config(&board, &config);
        assert_eq!(result.best_move, Some("e6e5".parse().unwrap()));
        assert!(result.score > 500);
    }

    #[test]
    fn handles_terminal_positions_and_zero_depth() {
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);

        // Depth 0 evaluates the position without searching moves.
        let start = Board::default();
        let res0 = engine.search(&start, 0);
        assert_eq!(res0.best_move, None);
        assert_eq!(res0.score, 0);
        assert_eq!(res0.depth, 0);
        assert_eq!(res0.nodes, 1);

        // Stalemate: Black king on h8, White king on f7, White queen on g6 (Black to move).
        let stalemate: Board = "7k/5K2/6Q1/8/8/8/8/8 b - - 0 1".parse().unwrap();
        let res_stale = engine.search(&stalemate, 2);
        assert_eq!(res_stale.best_move, None);
        assert_eq!(res_stale.score, 0);

        // Checkmate: White mated (Fool's mate).
        let mut mated = Board::default();
        for mv in ["f2f3", "e7e5", "g2g4", "d8h4"] {
            mated.play(mv.parse().unwrap());
        }
        let res_mate = engine.search(&mated, 2);
        assert_eq!(res_mate.best_move, None);
        assert_eq!(res_mate.score, NEG_INF);
    }

    #[test]
    fn respects_searchmoves_filter() {
        let board = Board::default();
        let mut engine = SearchEngine::new(MinimaxSearcher::new(), SimpleEvaluator);
        let target: Move = "e2e4".parse().unwrap();
        let config = SearchConfig {
            max_depth: Some(1),
            searchmoves: vec![target],
            ..SearchConfig::default()
        };

        let result = engine.search_with_config(&board, &config);
        assert_eq!(result.best_move, Some(target));
    }

    #[test]
    fn iterative_deepening_exceeds_depth_three_with_time_budget() {
        // Simple endgame position where alpha-beta easily deepens past depth 3.
        let board: Board = "6k1/5p2/8/8/8/8/5P2/4R1K1 w - - 0 1".parse().unwrap();
        let mut searcher = MinimaxSearcher::new();
        let stop = StopSignal::new();
        let mut reported_depths = Vec::new();

        let result = searcher.search_with_info(
            &board,
            &SimpleEvaluator,
            &SearchConfig::with_time(Duration::from_millis(120)),
            &stop,
            &mut |info| reported_depths.push(info.depth),
        );

        assert!(result.best_move.is_some());
        assert!(
            result.depth > 3,
            "expected depth > 3, got {} (reported: {:?})",
            result.depth,
            reported_depths
        );
        assert_eq!(reported_depths[0], 1);
    }

    #[test]
    fn stops_immediately_when_stop_signal_is_pre_set() {
        let mut searcher = MinimaxSearcher::new();
        let stop = StopSignal::new();
        stop.stop();

        let result = searcher.search(
            &Board::default(),
            &SimpleEvaluator,
            &SearchConfig::with_depth(4),
            &stop,
        );
        assert!(result.stopped);
        assert_eq!(result.nodes, 0);
        assert_eq!(result.best_move, None);
    }

    #[test]
    fn stops_in_progress_search_from_another_thread() {
        let mut searcher = MinimaxSearcher::new();
        let stop = StopSignal::new();
        let stop_clone = stop.clone();

        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            stop_clone.stop();
        });

        let result = searcher.search(
            &Board::default(),
            &SimpleEvaluator,
            &SearchConfig::infinite(),
            &stop,
        );
        stopper.join().unwrap();

        assert!(result.stopped);
        assert!(result.nodes > 0);
        assert!(result.best_move.is_some());
    }
}
