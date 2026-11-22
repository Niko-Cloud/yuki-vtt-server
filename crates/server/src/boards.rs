//! A board (a scene): map pictures laid on it under everything else, with the board's own grid
//! (the website's `packages/engine/src/board.ts`).

use serde::{Deserialize, Serialize};

pub const MOST_BOARD: f64 = 16_384.0;
pub const MOST_MAP_ITEMS: usize = 50;

/// A map picture on a board: a library picture of kind "map", placed and sized in the board's
/// pixels, in quarter turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapItem {
    pub id: String,
    pub asset: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

/// A map picture's own grid, as the DM lined it up: pixels per square and where lines start.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureGrid {
    pub cell: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

/// How big a picture is drawn on a board so its squares match the board's.
pub fn item_size(
    width: f64,
    height: f64,
    grid: Option<&PictureGrid>,
    board_cell: f64,
) -> (f64, f64) {
    let scale = board_cell / grid.map_or(board_cell, |g| g.cell);
    (width * scale, height * scale)
}

/// Where an item reaches, as turned.
pub fn item_box(item: &MapItem) -> (f64, f64) {
    if matches!(item.rotation, Some(90 | 270)) {
        (item.height, item.width)
    } else {
        (item.width, item.height)
    }
}

/// A board grown, never shrunk, to hold its map pictures.
pub fn grown(width: u32, height: u32, maps: &[MapItem]) -> (u32, u32) {
    let (mut w, mut h) = (f64::from(width), f64::from(height));
    for item in maps {
        let (across, down) = item_box(item);
        w = w.max((item.x + across).ceil());
        h = h.max((item.y + down).ceil());
    }
    (w.min(MOST_BOARD) as u32, h.min(MOST_BOARD) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_grows_like_the_website() {
        let grid = PictureGrid {
            cell: 50.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        assert_eq!(item_size(1000.0, 500.0, Some(&grid), 70.0), (1400.0, 700.0));
        assert_eq!(item_size(1000.0, 500.0, None, 70.0), (1000.0, 500.0));
        let item = MapItem {
            id: "m".into(),
            asset: "a".into(),
            x: 1400.0,
            y: 0.0,
            width: 700.0,
            height: 980.0,
            rotation: None,
            locked: None,
        };
        assert_eq!(grown(1400, 980, std::slice::from_ref(&item)), (2100, 980));
        assert_eq!(grown(3000, 980, std::slice::from_ref(&item)), (3000, 980));
        let turned = MapItem {
            rotation: Some(90),
            ..item
        };
        assert_eq!(grown(100, 100, &[turned]), (2380, 700));
    }
}
