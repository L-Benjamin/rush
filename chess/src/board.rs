use std::fmt;
use std::str::FromStr;

use crate::attacks;
use crate::bitboard::BitBoard;
use crate::castle_rights::CastleMask;
use crate::castle_rights::CastleRights;
use crate::color::Color;
use crate::cuckoo;
use crate::en_passant::EnPassantSquare;
use crate::errors::ParseFenError;
use crate::moves::Move;
use crate::piece::Piece;
use crate::square::Square;
use crate::zobrist::Zobrist;

//#################################################################################################
//
//                                    struct StateInfo
//
//#################################################################################################

// The state of the board at a given turn.
#[derive(Clone, Debug)]
struct StateInfo {
    side_to_move: Color,
    halfmove: u8,
    checkers: BitBoard,
    pinned: BitBoard,
    castle_rights: CastleRights,
    ep_square: EnPassantSquare,
    zobrist: Zobrist,
}

//#################################################################################################
//
//                                      struct Occupancy
//
//#################################################################################################

/// A struct holding all necessary occupancy informations of a boad.
#[derive(Clone, Default, Debug)]
pub struct Occupancy {
    all: BitBoard,
    colored: [BitBoard; 2],
}

// ================================ pub impl

impl Occupancy {
    /// The monochrome occupancy bitboard.
    #[inline]
    pub fn all(&self) -> BitBoard {
        self.all
    }

    /// The colored occupancy bitboards.
    #[inline]
    pub fn colored(&self, color: Color) -> BitBoard {
        self.colored[color.idx()]
    }
}

// ================================ impl

impl Occupancy {
    // Update the occupancy bitboard with a given color and mask.
    #[inline]
    fn update(&mut self, color: Color, mask: BitBoard) {
        self.all ^= mask;
        self.colored[color.idx()] ^= mask;
    }
}


//#################################################################################################
//
//                                         struct Board
//
//#################################################################################################

/// A struct representing a complete position of chess, with many accessers and
/// methods to manipulate it.
#[derive(Clone, Debug)]
pub struct Board {
    fullmove: u16,

    bitboards: [[BitBoard; 6]; 2],
    mailbox: [Option<(Color, Piece)>; 64],
    occ: Occupancy,

    state: StateInfo,
    prev_states: Vec<StateInfo>,
}

// ================================ pub impl

impl Board {
    /// Gets the bitboard corresponding to that color and piece type.
    #[inline]
    pub fn get_bitboard(&self, color: Color, piece: Piece) -> BitBoard {
        self.bitboards[color.idx()][piece.idx()]
    }

    /// Gets the (maybe) piece and it's color at that square.
    #[inline]
    pub fn get_piece(&self, sq: Square) -> Option<(Color, Piece)> {
        self.mailbox[sq.idx()]
    }

    /// Returns the occupancy object associated to that board.
    #[inline]
    pub fn get_occupancy(&self) -> &Occupancy {
        &self.occ
    }

    /// Returns the square the king of the side to move is occupying. 
    #[inline]
    pub fn get_king_sq(&self) -> Square {
        self.get_bitboard(self.state.side_to_move, Piece::King).as_square_unchecked()
    }

    /// Clears the history of the board, making it impossible to 
    /// undo the previous moves but freeing a bit of memory.
    #[inline]
    pub fn clear_history(&mut self) {
        self.prev_states.clear()
    }

    /// Returns true if that pseudo-legal move is legal.
    /// In particular, checks whether or not the move does not violate pin
    /// (or double pin for en passant moves), or, if it is a castling move,
    /// whether or not the squares the king traverses are safe.
    pub fn is_legal(&self, mv: Move) -> bool {
        let (from, to) = mv.squares();

        if mv.is_castle() {
            // If the move is castle, we must check that the squares the king
            // passes are safe.
            let can_castle = |sq1, sq2| {
                (self.attackers_to(sq1) | self.attackers_to(sq2)).empty()
            };

            return match to {
                Square::G1 => can_castle(Square::H1, Square::F1),
                Square::G8 => can_castle(Square::H8, Square::F8),
                Square::C1 => can_castle(Square::A1, Square::D1),
                Square::C8 => can_castle(Square::A8, Square::D8),
                _ => unreachable!(),
            };
        } else if mv.is_en_passant() {
            // If the move is en passant, we must check that there is no double pin.
            let ep_square = self.state.ep_square.unwrap();
            let rank = ep_square.rank();
            let king_sq = self.get_king_sq();

            // If the king is on the same rank as the ep square (very rare).
            if rank.contains(king_sq) {
                let them = self.state.side_to_move.invert();
                // For every rook on that very same rank.
                for rook_sq in (self.get_bitboard(them, Piece::Rook) & rank).iter_squares() {
                    let between = BitBoard::between(king_sq, rook_sq);
                    // If the ep square is exactly between the king and the rook, 
                    // and there is nothing else than the two pawns, then it is an
                    // (incredibly rare) double pin.
                    if between.contains(ep_square) && between.count() == 2 {
                        return false;
                    }
                }
            }
        }

        // Any move is valid if the piece is not pinned or if it is moving in the squares 
        // projected from the king and onward.
        !self.state.pinned.contains(from) || BitBoard::ray_mask(self.get_king_sq(), from).contains(to)
    }

    /// Returns true if that random move is pseudo-legal. Only assumes that the
    /// move was created through one of the Move type's metods.
    pub fn is_pseudo_legal(&self, mv: Move) -> bool {
        macro_rules! verify {($cond: expr) => {if !($cond) {return false;}}}

        let (from, to) = mv.squares();

        // Verify that the from square is occupied.
        if let Some((color, piece)) = self.get_piece(from) {
            // Verify it is one of our pieces.
            verify!(color == self.state.side_to_move);

            // Verify to square occupied <=> move is a capture and the square 
            // is occupied by the piece stored in the move.
            if let Some((color, piece)) = self.get_piece(to) {
                verify!(mv.is_capture() && color != self.state.side_to_move && piece == mv.get_capture());
            } else {
                verify!(!mv.is_capture());
            }

            let checkers = self.state.checkers;

            // Special case for the king.
            if piece == Piece::King {
                // If the move is castling.
                if mv.is_castle() {
                    let can_castle = |king_sq, rook_sq, mask| {
                        self.get_piece(rook_sq) == Some((color, Piece::Rook)) &&
                        self.is_path_clear(king_sq, rook_sq) && 
                        self.state.castle_rights.has(mask)
                    };

                    // The king must not be in check and the path between the king and the rook must be clear.
                    // Plus, there must be a rook on the rook square and we must possess the adequate
                    // castling rights.
                    return checkers.empty() && match color {
                        Color::White => match (from, to) {
                            (Square::E1, Square::G1) => can_castle(Square::E1, Square::H1, CastleMask::WhiteOO),
                            (Square::E1, Square::C1) => can_castle(Square::E1, Square::A1, CastleMask::WhiteOOO),
                            _ => return false,
                        },
                        Color::Black => match (from, to) {
                            (Square::E8, Square::G8) => can_castle(Square::E8, Square::H8, CastleMask::BlackOO),
                            (Square::E8, Square::C8) => can_castle(Square::E8, Square::A8, CastleMask::BlackOOO),
                            _ => return false,
                        },
                    };
                }

                // If it is a regular move, the square the king is moving to must safe.
                // Plus, the move is a valid king move.
                return self.attackers_to(to).empty() && attacks::king(from).contains(to);
            } else {
                // The move can't be a castle if the piece moving is not the king.
                verify!(!mv.is_castle());

                // If there are any checkers.
                match checkers.count() {
                    // One checker, the piece moving must either block or capture the enemy piece.
                    1 => {
                        let blocking_zone = BitBoard::between(self.get_king_sq(), checkers.as_square_unchecked());
                        verify!((blocking_zone | checkers).contains(to));
                    },
                    // Two checkers, the piece moving must be the king.
                    2 => return false,
                    _ => (),
                }
            }

            // Special case for pawns.
            if piece == Piece::Pawn {
                if mv.is_en_passant() {
                    // There must be an en passant square.
                    verify!(self.state.ep_square.is_some());
                    let ep_square = self.state.ep_square.unwrap();

                    // The ep square must between the move's squares.
                    return ep_square == Square::from((to.x(), from.y())) &&
                        attacks::pawns(color, from).contains(to);
                } else {
                    // If the move is a promotion, it must go to the first or last rank.
                    verify!(to.y() == 0 || to.y() == 7 || !mv.is_promote());

                    // Verify that the move is legal for a pawn.
                    if mv.is_capture() {
                        return attacks::pawns(color, from).contains(to)
                    } else {
                        return if mv.is_double_push() {
                            attacks::pawn_double_push(color, from)
                        } else {
                            attacks::pawn_push(color, from)
                        } == Some(to)
                    };
                }
            } else {
                // If the piece is not a pawn, the move can't be of any of those types. 
                verify!(!mv.is_en_passant() && !mv.is_double_push() && !mv.is_promote());
            }

            // For any other piece, verify the move would be valid on an empty board.
            let occ = self.occ.all;
            return match piece {
                Piece::Rook => attacks::rook(from, occ),
                Piece::Knight => attacks::knight(from),
                Piece::Bishop => attacks::bishop(from, occ),
                Piece::Queen => attacks::queen(from, occ),
                _ => unreachable!(),
            }.contains(to);
        }

        false
    }

    /// Do the move without checking anything about it's legality.
    /// Returns true if the move is irreversible.
    pub fn do_move(&mut self, mv: Move) -> bool {
        // Store previous state and increment fullmove counter.
        self.prev_states.push(self.state.clone());
        if self.state.side_to_move == Color::Black {
            self.fullmove += 1;
        }

        // Invert the side to move.
        self.state.side_to_move = self.state.side_to_move.invert();

        // Extract base move infos and remove piece from it's starting position.
        let (from, to) = mv.squares();
        let (color, mut piece) = self.remove_piece::<true>(from);

        // Determine if the move is reversible or not.
        let reversible = mv.is_quiet() && piece != Piece::Pawn;

        if mv.is_castle() {
            // If the move is castling, move the rook as well.
            match to {
                Square::G1 => self.displace_piece::<true>(Square::H1, Square::F1),
                Square::G8 => self.displace_piece::<true>(Square::H8, Square::F8),
                Square::C1 => self.displace_piece::<true>(Square::A1, Square::D1),
                Square::C8 => self.displace_piece::<true>(Square::A8, Square::D8),
                _ => unreachable!(),
            };
        } else if mv.is_en_passant() {
            // If the move is en passant, remove the pawn at the en passant square.
            self.remove_piece::<true>(self.state.ep_square.unwrap());
        } else {
            // If the move is a capture, remove the enemy piece from the destination square.
            if mv.is_capture() {
                self.remove_piece::<true>(to);
            }
    
            // If the move is a promotion, 
            if mv.is_promote() {
                piece = mv.get_promote();
            }
        }

        // Finally, place the piece at it's destination.
        self.place_piece::<true>(color, piece, to);

        // Determine checkers and pinned bitboard.
        self.state.checkers = self.checkers();
        self.state.pinned = self.pinned();

        // Update castling rights and en passant square.
        self.state.castle_rights.update(from, to);
        if mv.is_double_push() {
            self.state.ep_square = EnPassantSquare::Some(to);
        } else {
            self.state.ep_square = EnPassantSquare::None;
        }

        // Update the halfmove clock.
        if reversible {
            self.state.halfmove += 1;
        } else {
            self.state.halfmove = 0;
        }

        // Invert the zobrist key, as we change color.
        self.state.zobrist = !self.state.zobrist;

        reversible
    }

    /// Undoes the move, reverting the board to it's previous state.
    pub fn undo_move(&mut self, mv: Move) {
        // Them color.
        let them = self.state.side_to_move;

        // Restore the previous state and decrement the fullmove counter.
        self.state = self.prev_states.pop().unwrap();
        if self.state.side_to_move == Color::Black {
            self.fullmove -= 1;
        }

        // Extract basic move info and remove the piece from it's destination.
        let (from, to) = mv.squares();
        let (color, mut piece) = self.remove_piece::<false>(to);

        if mv.is_castle() {
            // If the move was castling, move the rook back as well.
            match to {
                Square::G1 => self.displace_piece::<true>(Square::F1, Square::H1),
                Square::G8 => self.displace_piece::<true>(Square::F8, Square::H8),
                Square::C1 => self.displace_piece::<true>(Square::D1, Square::A1),
                Square::C8 => self.displace_piece::<true>(Square::D8, Square::A8),
                _ => unreachable!(),
            };
        } else if mv.is_en_passant() {
            // If the move was en passant, place the enemy pawn back as well.
            self.place_piece::<false>(them, Piece::Pawn, self.state.ep_square.unwrap());
        } else {
            // If the move was a capture, replace the taken enemy piece in it's place.
            if mv.is_capture() {
                self.place_piece::<false>(them, mv.get_capture(), to);
            }
    
            // If the move was a promotion, the original piece was a pawn.
            if mv.is_promote() {
                piece = Piece::Pawn;
            }
        }

        self.place_piece::<false>(color, piece, from);
    }

    /// Efficiently tests for an upcoming repetition on the line,
    /// using cuckoo hashing.
    pub fn test_upcoming_repetition(&self) -> bool {
        if self.state.halfmove < 3 {
            return false;
        }

        let cur_zobrist = self.state.zobrist;
        let nth_zobrist = |n: u8| {
            self.prev_states[self.prev_states.len() - usize::from(n)].zobrist
        };

        let mut other = !(cur_zobrist ^ nth_zobrist(1));

        for d in (3..=self.state.halfmove).step_by(2) {
            other ^= !(nth_zobrist(d-1) ^ nth_zobrist(d));

            if other != Zobrist::ZERO {
                continue;
            }

            let diff = cur_zobrist ^ nth_zobrist(d);

            if cuckoo::is_hash_of_legal_move(self, diff) {
                return true;
            }
        }

        false
    }

    /// Parses the move, checking the legality of the move.
    pub fn parse_move(&self, s: &str) -> Result<Move, ParseFenError> {
        let mv = match s.len() {
            4 => {
                let from = Square::from_str(&s[0..2])?;
                let to = Square::from_str(&s[2..4])?;

                match self.get_piece(from) {
                    Some((_, Piece::Pawn)) => {
                        if from.x() == to.x() {
                            if (to.y() - from.y()).abs() == 2 {
                                Move::double_push(from, to)
                            } else {
                                Move::quiet(from, to)
                            }
                        } else if let Some((_, capture)) = self.get_piece(to) {
                            Move::capture(from, to, capture)
                        } else {
                            Move::en_passant(from, to)
                        }
                    },
                    Some((_, Piece::King)) => {
                        if (to.x() - from.x()).abs() == 2 {
                            Move::castle(from, to)
                        } else if let Some((_, capture)) = self.get_piece(to) {
                            Move::capture(from, to, capture)
                        } else {
                            Move::quiet(from, to)
                        }
                    },
                    _ => {
                        if let Some((_, capture)) = self.get_piece(to) {
                            Move::capture(from, to, capture)
                        } else {
                            Move::en_passant(from, to)
                        }
                    },
                }
            },
            5 => {
                let from = Square::from_str(&s[0..2])?;
                let to = Square::from_str(&s[2..4])?;

                let promote = match s.chars().nth(4).unwrap() {
                    'r' => Piece::Rook,
                    'n' => Piece::Knight,
                    'b' => Piece::Bishop,
                    'q' => Piece::Queen,
                    c => return Err(ParseFenError::new(format!("unrecognized promotion: \"{}\", valid promotions are: \"rnbq\"", c))),
                };
    
                if let Some((_, capture)) = self.get_piece(to) {
                    Move::promote_capture(from, to, capture, promote)
                } else {
                    Move::promote(from, to, promote)
                }
            },
            _ => return Err(ParseFenError::new("a move should be encoded in pure algebraic coordinate notation")),
        };

        if self.is_pseudo_legal(mv) && self.is_legal(mv) {
            Ok(mv)
        } else {
            Err(ParseFenError::new(format!("move is illegal in this context: \"{}\"", s)))
        }
    }

    /// Pretty-prints the board to stdout, using utf-8 characters
    /// to represent the pieces
    pub fn pretty_print(&self) -> String {
        const CHARS: [[char; 6]; 2] = [
            ['♙', '♖', '♘', '♗', '♕', '♔'],
            ['♟', '♜', '♞', '♝', '♛', '♚'],
        ];

        let mut res = String::new();

        res += "  a b c d e f g h\n";
        for y in (0..8).rev() {
            res += &(y + 1).to_string();
            for x in 0..8 {
                res.push(' ');
                if let Some((color, piece)) = self.get_piece(Square::from((x, y))) {
                    res.push(CHARS[color.idx()][piece.idx()]);
                } else {
                    res.push(' ');
                }
            }
            if y != 0 {
                res.push('\n');
            }
        }

        res
    } 
}

// ================================ pub(crate) impl

impl Board {
    /// Returns true from and to are not aligned, or if the squares
    /// between them are empty.
    #[inline]
    pub(crate) fn is_path_clear(&self, from: Square, to: Square) -> bool {
        (BitBoard::between(from, to) & self.occ.all).empty()
    }
}

// ================================ impl

impl Board {
    #[inline]
    fn place_piece<const ZOBRIST: bool>(&mut self, color: Color, piece: Piece, sq: Square) {
        self.bitboards[color.idx()][piece.idx()] ^= sq.into();
        self.mailbox[sq.idx()] = Some((color, piece));

        let mask = sq.into();
        self.occ.update(color, mask);

        if ZOBRIST {
            self.state.zobrist ^= Zobrist::from((color, piece, sq));
        }
    }

    #[inline]
    fn remove_piece<const ZOBRIST: bool>(&mut self, sq: Square) -> (Color, Piece) {
        let (color, piece) = self.mailbox[sq.idx()].unwrap();
        self.bitboards[color.idx()][piece.idx()] ^= sq.into();
        self.mailbox[sq.idx()] = None;

        let mask = sq.into();
        self.occ.update(color, mask);

        if ZOBRIST {
            self.state.zobrist ^= Zobrist::from((color, piece, sq));
        }

        (color, piece)
    }

    #[inline]
    fn displace_piece<const ZOBRIST: bool>(&mut self, from: Square, to: Square) -> (Color, Piece) {
        let (color, piece) = self.remove_piece::<ZOBRIST>(from);
        self.place_piece::<ZOBRIST>(color, piece, to);
        (color, piece)
    }

    /// The bitboard of the checkers to the current king.
    #[inline]
    fn checkers(&self) -> BitBoard {
        self.attackers_to(self.get_king_sq())
    }

    /// The bitboard of the currently pinned pieces.
    #[inline]
    fn pinned(&self) -> BitBoard {
        let us = self.state.side_to_move;
        let occ_us = self.occ.colored(us);
        let them = us.invert();
        let queens = self.get_bitboard(them, Piece::Queen);
        let king_sq = self.get_king_sq();

        let mut pinned = BitBoard::EMPTY;

        for sq in (self.get_bitboard(them, Piece::Rook) | queens).iter_squares() {
            let between = BitBoard::between_straight(king_sq, sq);
            if (between & self.occ.all).count() == 1 {
                pinned |= between & occ_us;
            }
        }

        for sq in (self.get_bitboard(them, Piece::Bishop) | queens).iter_squares() {
            let between = BitBoard::between_diagonal(king_sq, sq);
            if (between & self.occ.all).count() == 1 {
                pinned |= between & occ_us;
            }
        }

        pinned
    }

    // Returns the bitboard of all the attackers to that square. Does not take
    // en passant into account.
    #[inline]
    fn attackers_to(&self, sq: Square) -> BitBoard {
        let us   = self.state.side_to_move;
        let them = us.invert();
        let occ  = self.occ.all;

        attacks::pawns(us, sq) & self.get_bitboard(them, Piece::Pawn) 
        | attacks::rook(sq, occ) & self.get_bitboard(them, Piece::Rook) 
        | attacks::knight(sq) & self.get_bitboard(them, Piece::Knight) 
        | attacks::bishop(sq, occ) & self.get_bitboard(them, Piece::Bishop) 
        | attacks::queen(sq, occ) & self.get_bitboard(them, Piece::Queen) 
        | attacks::king(sq) & self.get_bitboard(them, Piece::King)
    }
}

// ================================ traits impl

impl Default for Board {
    /// Returns the default position of chess.
    fn default() -> Board {
        Board::from_str("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").unwrap()
    }
}

impl fmt::Display for Board {
    /// Formats the board to it's fen representation.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        macro_rules! write_if_not_zero {
            ($i: expr) => {
                if $i != 0 {
                    write!(f, "{}", ('0' as u8 + $i) as char)?
                }
            };
        }
        
        for y in (0..8).rev() {
            let mut streak = 0;

            for x in 0..8 {
                if let Some((color, piece)) = self.get_piece(Square::from((x, y))) {
                    write_if_not_zero!(streak);
                    write!(f, "{}", match color {
                        Color::White => piece.to_string().to_uppercase(),
                        Color::Black => piece.to_string(),
                    })?;
                    streak = 0;
                } else {
                    streak += 1;
                }
            }

            write_if_not_zero!(streak);
            if y != 0 {
                write!(f, "/")?;
            }
        }

        write!(
            f, 
            " {} {} {} {} {}", 
            self.state.side_to_move,
            self.state.castle_rights,
            self.state.ep_square,
            self.state.halfmove,
            self.fullmove,
        )?;

        Ok(())
    }
}

impl FromStr for Board {
    type Err = ParseFenError;

    /// Tries to parse a board from a string in fen representation.
    fn from_str(s: &str) -> Result<Board, ParseFenError> {        
        let split = s.split(' ').into_iter().collect::<Vec<_>>();
        
        if split.len() != 6 {
            return Err(ParseFenError::new(format!("not enough arguments in fen string {:?}", s)));
        }

        let side_to_move = Color::from_str(split[1])?;
        let castle_rights = CastleRights::from_str(split[2])?;
        let ep_square = EnPassantSquare::from_str(split[3])?;
        let halfmove = u8::from_str(split[4])?;
        let fullmove = u16::from_str(split[5])?;

        let mut board = Board {
            fullmove,
            bitboards: [[BitBoard::EMPTY; 6]; 2],
            mailbox: [None; 64],
            occ: Occupancy::default(),
            state: StateInfo {
                side_to_move,
                halfmove,
                checkers: BitBoard::EMPTY,
                pinned: BitBoard::EMPTY,
                castle_rights,
                ep_square,
                zobrist: Zobrist::default(),
            },
            prev_states: Vec::new(),
        };

        let ranks = split[0].split('/').into_iter().collect::<Vec<_>>();

        if ranks.len() != 8 {
            return Err(ParseFenError::new(format!("not enough ranks in fen board {:?}", s)));
        }

        for (y, rank) in ranks.iter().enumerate() {
            let mut x = 0;
            let y = (7 - y) as i8;

            for c in rank.chars() {
                match c {
                    '1'..='8' => x += c as i8 - '1' as i8,
                    _ => {
                        let (color, piece) = Piece::from_char(c)?;
                        let sq = Square::from((x, y));
                        board.get_bitboard(Color::White, Piece::Pawn);
                        board.place_piece::<true>(color, piece, sq);
                    }
                }

                x += 1;
                if x > 8 {
                    return Err(ParseFenError::new(format!("rank {:?} is too large in fen string", rank)))
                }
            }

            if x != 8 {
                return Err(ParseFenError::new(format!("rank {:?} is too small fen string", rank)))
            }
        }

        board.state.checkers = board.checkers();
        board.state.pinned   = board.pinned();

        Ok(board)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changed castle, in case it breaks: first is black kingside and second is white queen side
    #[test]
    fn do_and_undo() {
        use Square::*;

        crate::init();

        let moves = vec![
            Move::double_push(D2, D4),
            Move::quiet(B8, C6),
            Move::quiet(D4, D5),
            Move::quiet(G7, G6),
            Move::quiet(C1, H6),
            Move::capture(F8, H6, Piece::Bishop),
            Move::quiet(D1, D3),
            Move::double_push(E7, E5),
            Move::en_passant(D5, E6),
            Move::quiet(G8, F6),
            Move::quiet(B1, C3),
            Move::castle(E8, G8),
            Move::quiet(E2, E5),
            Move::double_push(B7, B5),
            Move::castle(E1, C1),
            Move::quiet(B5, B4),
            Move::capture(E6, D7, Piece::Pawn),
            Move::quiet(B4, B3),
            Move::promote_capture(D7, C8, Piece::Bishop, Piece::Knight),
            Move::capture(B3, A2, Piece::Pawn),
            Move::quiet(C8, B6),
            Move::promote(A2, A1, Piece::Queen),
        ];

        let mut board = Board::default();
        
        for &mv in moves.iter() {
            board.do_move(mv);
        }

        for &mv in moves.iter().rev() {
            board.undo_move(mv);
        }

        let default = Board::default();

        for color in Color::COLORS {
            for piece in Piece::PIECES {
                assert_eq!(
                    default.get_bitboard(color, piece),
                    board.get_bitboard(color, piece),
                )
            }
        }
        assert_eq!(default.occ.all, board.occ.all);
        assert_eq!(default.occ.colored, board.occ.colored);
        assert_eq!(default.state.zobrist, board.state.zobrist);
    }
}