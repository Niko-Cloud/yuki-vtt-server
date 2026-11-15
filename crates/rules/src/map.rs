//! The map's square grid: which cell a point is in, and how far apart two cells are.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapGrid {
    /// How wide one cell is, in the picture's pixels.
    pub size: u32,
    /// Where the first full cell starts, for a picture whose drawn grid doesn't start at 0.
    pub offset_x: u32,
    pub offset_y: u32,
    pub feet_per_cell: u32,
    pub show: bool,
    /// How a diagonal step counts: every one a cell (the 5e rule, when unset), every other
    /// one two cells (5-10-5), the real distance, or always two cells (only straight steps,
    /// as on a city's streets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagonals: Option<Diagonals>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Diagonals {
    Five,
    Alternate,
    Real,
    Manhattan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub x: u32,
    pub y: u32,
}

/// How many cells the map holds across and down, counting from the grid's offset.
pub fn cells(width: u32, height: u32, grid: &MapGrid) -> (u32, u32) {
    let size = grid.size.max(1);
    (
        (width.saturating_sub(grid.offset_x) / size).max(1),
        (height.saturating_sub(grid.offset_y) / size).max(1),
    )
}

/// The cell under a point on the map (in the picture's pixels), kept inside the map so a
/// token of `size` cells still fits.
pub fn cell_at(width: u32, height: u32, grid: &MapGrid, px: f64, py: f64, size: u32) -> Cell {
    let (columns, rows) = cells(width, height, grid);
    let clamp = |value: f64, max: u32| (value.max(0.0) as u32).min(max);
    let step = f64::from(grid.size.max(1));
    Cell {
        x: clamp(
            ((px - f64::from(grid.offset_x)) / step).floor(),
            columns.saturating_sub(size),
        ),
        y: clamp(
            ((py - f64::from(grid.offset_y)) / step).floor(),
            rows.saturating_sub(size),
        ),
    }
}

/// Keeps a token of `size` cells on the map, for one moved or a map made smaller.
pub fn keep_inside(width: u32, height: u32, grid: &MapGrid, cell: Cell, size: u32) -> Cell {
    let (columns, rows) = cells(width, height, grid);
    Cell {
        x: cell.x.min(columns.saturating_sub(size)),
        y: cell.y.min(rows.saturating_sub(size)),
    }
}

/// Feet between two cells. Both editions count every square the same, diagonals too, so it's
/// the longer of the two directions.
pub fn distance(grid: &MapGrid, from: Cell, to: Cell) -> u32 {
    let (dx, dy) = (from.x.abs_diff(to.x), from.y.abs_diff(to.y));
    let (long, short) = (dx.max(dy), dx.min(dy));
    match grid.diagonals {
        // Every second diagonal step costs a cell more: 5, 10, 5, 10...
        Some(Diagonals::Alternate) => (long + short / 2) * grid.feet_per_cell,
        // As the crow flies, to the nearest foot.
        Some(Diagonals::Real) => {
            (f64::from(dx).hypot(f64::from(dy)) * f64::from(grid.feet_per_cell)).round() as u32
        }
        // Across and then down: a diagonal step is two.
        Some(Diagonals::Manhattan) => (dx + dy) * grid.feet_per_cell,
        _ => long * grid.feet_per_cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: MapGrid = MapGrid {
        size: 70,
        offset_x: 10,
        offset_y: 0,
        feet_per_cell: 5,
        show: true,
        diagonals: None,
    };

    #[test]
    fn counts_cells_from_the_offset() {
        assert_eq!(cells(710, 700, &GRID), (10, 10));
        assert_eq!(cells(50, 50, &GRID), (1, 1));
    }

    #[test]
    fn finds_the_cell_under_a_point_and_keeps_it_on_the_map() {
        assert_eq!(
            cell_at(710, 700, &GRID, 85.0, 150.0, 1),
            Cell { x: 1, y: 2 }
        );
        assert_eq!(
            cell_at(710, 700, &GRID, -40.0, 9000.0, 1),
            Cell { x: 0, y: 9 }
        );
        // A Large token can't hang over the edge.
        assert_eq!(
            cell_at(710, 700, &GRID, 700.0, 690.0, 2),
            Cell { x: 8, y: 8 }
        );
        assert_eq!(
            keep_inside(
                350,
                350,
                &MapGrid {
                    offset_x: 0,
                    ..GRID
                },
                Cell { x: 9, y: 1 },
                1
            ),
            Cell { x: 4, y: 1 }
        );
    }

    #[test]
    fn counts_diagonals_like_straight_lines() {
        assert_eq!(
            distance(&GRID, Cell { x: 0, y: 0 }, Cell { x: 3, y: 2 }),
            15
        );
    }

    #[test]
    fn counts_diagonals_each_way() {
        let (from, to) = (Cell { x: 0, y: 0 }, Cell { x: 4, y: 3 });
        assert_eq!(distance(&GRID, from, to), 20);
        let alternate = MapGrid {
            diagonals: Some(Diagonals::Alternate),
            ..GRID
        };
        assert_eq!(distance(&alternate, from, to), 25);
        let real = MapGrid {
            diagonals: Some(Diagonals::Real),
            ..GRID
        };
        assert_eq!(distance(&real, from, Cell { x: 1, y: 1 }), 7);
        let streets = MapGrid {
            diagonals: Some(Diagonals::Manhattan),
            ..GRID
        };
        assert_eq!(distance(&streets, from, to), 35);
    }
}
