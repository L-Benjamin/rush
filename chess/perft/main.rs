// cargo run --bin perft -- <depth> "<fen>" [<moves>]*
// cargo build --bin perft --release

use std::env::args;
use std::str::FromStr;

use chess::*;

// The perft algorithm, counting the number of leaf nodes
fn perft(game: &mut SearchGame<15>, depth: usize) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut nodes = 0;
    let mut move_gen = game.legals();

    loop {
        let mv = move_gen.next();
        if mv.is_none() {break}

        game.do_move(mv);
        nodes += perft(game, depth - 1);
        game.undo_move();
    }

    nodes
}

// How to use the perft script:
// cargo run --bin perft --release -- <depth> "<fen>" [<moves>]*
fn main() {
    let mut args = args();
    
    args.next().unwrap();

    let depth = usize::from_str(&args.next().expect("Cannot find depth argument")).expect("Cannot parse depth");

    let fen = args.next().expect("Cannot find FEN argument");
    let mut game = SearchGame::from_fen(&fen).expect("Cannot parse FEN");

    let mut moves = game.legals();
    let map = moves.to_map();

    let mut total = 0;

    if args.len() == 0 {
        for (s, mv) in map {
            game.do_move(mv);
            let count = perft(&mut game, depth-1);
            println!("{} {}", s, count);
            total += count;
            game.undo_move();
        }
    } else {
        for s in args {
            if let Some(mv) = map.get(&s) {
                game.do_move(*mv);
                let count = perft(&mut game, depth-1);
                println!("{} {}", s, count);
                total += count;
                game.undo_move();
            } else {
                panic!("Move seems invalid to me: {}", s);
            }
        }
    }

    println!("\n{}", total);
}