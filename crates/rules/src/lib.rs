//! The table rules Yuki VTT Server runs for a game: dice, initiative, hit points and the map
//! grid. Nothing here knows about books or character sheets; the website works those out and
//! hands the host the result.

pub mod changes;
pub mod dice;
pub mod encounter;
pub mod hit_points;
pub mod map;

pub use dice::{Advantage, Dice, Roll, parse_dice, roll, roll_detail};
