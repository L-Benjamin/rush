//extern crate wee_alloc;
extern crate chess;
extern crate wasm_bindgen;
extern crate wee_alloc;

use wasm_bindgen::prelude::*;

// Use the wee_alloc allocator instead of the std one to save space.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen(start)]
pub fn _start() -> Result<(), JsValue> {
    chess::init();
    Ok(())
}   