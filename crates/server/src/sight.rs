//! What a player sees on a map with dynamic fog, so tokens they can't see aren't sent to them
//! at all. The same rule as the website's (`packages/engine/src/vision.ts`, `playerSight`):
//! walls and closed doors stop sight, open doors and windows don't; on a bright map a token
//! sees all it has a line to, on a dark one only within its vision or where a light shines.
//! Deciding what shows needs only lines of sight, not the outlines the browser draws.

use crate::maps::{TableMap, Token, Wall};
use std::collections::HashMap;

/// Who is looking: their seats, whether the table shares sight, and each seat's darkvision.
pub struct Looking<'a> {
    pub seat_ids: &'a [String],
    pub shared: bool,
    pub darkvision: HashMap<String, Option<f64>>,
}

type Point = (f64, f64);
// A cone one way: where it points and how wide it is, in degrees.
type Cone = (f64, f64);

// A new cone's width until the DM sets one, as on the website.
const DEFAULT_CONE: f64 = 60.0;

fn cone_of(shape: Option<&str>, width: Option<f64>, angle: Option<f64>) -> Option<Cone> {
    if shape != Some("cone") {
        return None;
    }
    let width = width.unwrap_or(DEFAULT_CONE);
    (width < 360.0).then(|| (angle.unwrap_or(0.0), width))
}

fn within_cone(from: Point, (angle, width): Cone, p: Point) -> bool {
    if (p.0 - from.0).hypot(p.1 - from.1) < 1.0 {
        return true;
    }
    let toward = (p.1 - from.1).atan2(p.0 - from.0).to_degrees();
    let off = (((toward - angle) % 360.0 + 540.0) % 360.0 - 180.0).abs();
    off <= width / 2.0
}

fn blocking(walls: &[Wall]) -> Vec<(Point, Point)> {
    walls
        .iter()
        .filter(|w| !w.window && !w.door.as_ref().is_some_and(|d| d.open))
        .map(|w| ((w.a[0], w.a[1]), (w.b[0], w.b[1])))
        .collect()
}

// Where a ray from `from` along (dx, dy) first meets the segment, as a distance along it.
fn hit(from: Point, dx: f64, dy: f64, (p, q): (Point, Point)) -> f64 {
    let (ex, ey) = (q.0 - p.0, q.1 - p.1);
    let across = dx * ey - dy * ex;
    if across.abs() < 1e-12 {
        return f64::INFINITY;
    }
    let t = ((p.0 - from.0) * ey - (p.1 - from.1) * ex) / across;
    let u = ((p.0 - from.0) * dy - (p.1 - from.1) * dx) / across;
    if t > 0.0 && (0.0..=1.0).contains(&u) {
        t
    } else {
        f64::INFINITY
    }
}

// Whether nothing stops sight between two points; a wall a token stands against doesn't.
fn line_of_sight(walls: &[(Point, Point)], from: Point, to: Point) -> bool {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    if length < 1.0 {
        return true;
    }
    let (dx, dy) = (dx / length, dy / length);
    walls
        .iter()
        .all(|wall| hit(from, dx, dy, *wall) > length - 0.5)
}

fn center(map: &TableMap, token: &Token) -> Point {
    crate::fog::middle_of(map, token)
}

fn pixels(map: &TableMap, feet: f64) -> f64 {
    feet / f64::from(map.grid.feet_per_cell.max(1)) * f64::from(map.grid.size)
}

struct Viewer {
    at: Point,
    clear: f64,
    fade: f64,
    cone: Option<Cone>,
}

struct Shine {
    at: Point,
    reach: f64,
    cone: Option<Cone>,
}

struct Sight {
    bright: bool,
    viewers: Vec<Viewer>,
    lights: Vec<Shine>,
    walls: Vec<(Point, Point)>,
}

// The tokens a person sees through: their own seats' and those shared with them, or every
// party token when the table shares. Hidden tokens and tokens with no vision see nothing.
fn viewers_for<'m>(map: &'m TableMap, looking: &Looking) -> Vec<&'m Token> {
    map.tokens
        .iter()
        .filter(|t| {
            if t.hidden == Some(true) || t.vision.as_ref().is_some_and(|v| v.off == Some(true)) {
                return false;
            }
            if let Some(seat) = &t.seat_id
                && (looking.shared || looking.seat_ids.contains(seat))
            {
                return true;
            }
            t.shared_with
                .iter()
                .flatten()
                .any(|seat| looking.seat_ids.contains(seat))
        })
        .collect()
}

fn sight(map: &TableMap, viewers: &[&Token], looking: &Looking, carriers: &[&Token]) -> Sight {
    let bright = map.fog.light == "bright";
    let viewers = viewers
        .iter()
        .map(|token| {
            let darkvision = token
                .seat_id
                .as_ref()
                .and_then(|seat| looking.darkvision.get(seat).copied().flatten());
            let (range, falloff) = match (&token.vision, darkvision) {
                (Some(v), _) => (f64::from(v.range), f64::from(v.falloff)),
                (None, Some(range)) if range > 0.0 => (range, 10.0),
                _ => (0.0, 0.0),
            };
            let cone = token
                .vision
                .as_ref()
                .and_then(|v| cone_of(v.shape.as_deref(), v.width, token.facing));
            Viewer {
                at: center(map, token),
                clear: if bright {
                    f64::INFINITY
                } else {
                    pixels(map, range)
                },
                fade: if bright { 0.0 } else { pixels(map, falloff) },
                cone,
            }
        })
        .collect();
    let placed = map.lights.iter().map(|l| {
        let reach = f64::from(l.bright.max(l.dim));
        (
            (l.x, l.y),
            reach,
            cone_of(l.shape.as_deref(), l.width, l.angle),
        )
    });
    let carried = carriers.iter().filter_map(|t| {
        let light = t.light.as_ref()?;
        let reach = f64::from(light.bright.max(light.dim));
        Some((
            center(map, t),
            reach,
            cone_of(light.shape.as_deref(), light.width, t.facing),
        ))
    });
    let lights = placed
        .chain(carried)
        .filter(|(_, reach, _)| *reach > 0.0)
        .map(|(at, reach, cone)| Shine {
            at,
            reach: pixels(map, reach),
            cone,
        })
        .collect();
    Sight {
        bright,
        viewers,
        lights,
        walls: blocking(&map.walls_with_rooms()),
    }
}

fn seen(sight: &Sight, p: Point) -> bool {
    let lit = || {
        sight.lights.iter().any(|l| {
            (p.0 - l.at.0).hypot(p.1 - l.at.1) <= l.reach
                && l.cone.is_none_or(|c| within_cone(l.at, c, p))
                && line_of_sight(&sight.walls, l.at, p)
        })
    };
    sight.viewers.iter().any(|v| {
        if v.cone.is_some_and(|c| !within_cone(v.at, c, p)) {
            return false;
        }
        if !line_of_sight(&sight.walls, v.at, p) {
            return false;
        }
        sight.bright || (p.0 - v.at.0).hypot(p.1 - v.at.1) <= v.clear + v.fade || lit()
    })
}

/// Which tokens show to this player now: their own and those shared with them always (even
/// one the DM blinded, so they can still move it), others only where they see. A light
/// carried by a token shows only once its carrier does: a monster's torch in the dark stays
/// hidden until the monster itself shows.
pub fn shown_ids(map: &TableMap, looking: &Looking) -> Vec<String> {
    let viewers = viewers_for(map, looking);
    let own = |t: &Token| {
        t.seat_id
            .as_ref()
            .is_some_and(|s| looking.seat_ids.contains(s))
            || t.shared_with
                .iter()
                .flatten()
                .any(|s| looking.seat_ids.contains(s))
            || viewers.iter().any(|v| v.id == t.id)
    };
    let base = sight(map, &viewers, looking, &viewers);
    let lit: Vec<&Token> = map
        .tokens
        .iter()
        .filter(|t| {
            t.light.is_some() && t.hidden != Some(true) && (own(t) || seen(&base, center(map, t)))
        })
        .collect();
    let full = sight(map, &viewers, looking, &lit);
    map.tokens
        .iter()
        .filter(|t| own(t) || seen(&full, center(map, t)))
        .map(|t| t.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn room_edges_are_walls_less_a_door_along_one() {
        use crate::maps::{Door, FogArea, room_edge_walls};
        let hall = FogArea {
            id: "hall".into(),
            reveal: None,
            points: vec![[0.0, 0.0], [140.0, 0.0], [140.0, 140.0], [0.0, 140.0]],
        };
        assert_eq!(room_edge_walls(std::slice::from_ref(&hall), &[]).len(), 4);
        let door = Wall {
            id: "d".into(),
            a: [50.0, 140.0],
            b: [90.0, 140.0],
            door: Some(Door {
                open: true,
                locked: false,
                secret: false,
            }),
            window: false,
        };
        let bottom: Vec<(f64, f64)> = room_edge_walls(&[hall], &[door])
            .into_iter()
            .filter(|w| w.a[1] == 140.0 && w.b[1] == 140.0)
            .map(|w| (w.a[0], w.b[0]))
            .collect();
        assert_eq!(bottom, vec![(140.0, 90.0), (50.0, 0.0)]);
    }

    // The website's cases (vision.test.ts): ten by ten cells of 50 pixels, 5 feet each, with a
    // wall down the middle and a door in it.
    fn map(patch: serde_json::Value) -> TableMap {
        let mut base = json!({
            "width": 500, "height": 500,
            "grid": { "size": 50, "offsetX": 0, "offsetY": 0, "feetPerCell": 5, "show": true },
            "image": null, "tokens": [], "props": [], "drawings": [],
            "fog": { "mode": "dynamic", "light": "bright", "remember": false },
            "walls": [
                { "id": "w1", "a": [250, 0], "b": [250, 200] },
                { "id": "d1", "a": [250, 200], "b": [250, 300], "door": { "open": false } },
                { "id": "w2", "a": [250, 300], "b": [250, 500] }
            ],
            "lights": [], "notes": [], "templates": []
        });
        for (key, value) in patch.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }
    fn tam() -> serde_json::Value {
        json!({ "id": "t", "name": "Tam", "x": 1, "y": 4, "size": 1, "color": "#86d5ed", "seatId": "s1" })
    }
    fn ogre() -> serde_json::Value {
        json!({ "id": "g", "name": "Ogre", "x": 8, "y": 4, "size": 1, "color": "#ff9090" })
    }
    fn ids(map: &TableMap, darkvision: &[(&str, f64)]) -> Vec<String> {
        let seats = vec!["s1".to_string()];
        let looking = Looking {
            seat_ids: &seats,
            shared: false,
            darkvision: darkvision
                .iter()
                .map(|(s, d)| (s.to_string(), Some(*d)))
                .collect(),
        };
        shown_ids(map, &looking)
    }
    fn dark(tokens: serde_json::Value, lights: serde_json::Value) -> TableMap {
        map(json!({
            "fog": { "mode": "dynamic", "light": "dark", "remember": false },
            "walls": [], "tokens": tokens, "lights": lights
        }))
    }

    #[test]
    fn shows_a_player_only_the_tokens_they_see_now_like_the_website() {
        // Behind a closed door the ogre is hidden; through the open door it shows.
        assert_eq!(ids(&map(json!({ "tokens": [tam(), ogre()] })), &[]), ["t"]);
        let mut open = map(json!({ "tokens": [tam(), ogre()] }));
        open.walls[1].door.as_mut().unwrap().open = true;
        assert_eq!(ids(&open, &[]), ["t", "g"]);
        // In the dark: only within darkvision, or where a light shines.
        assert_eq!(ids(&dark(json!([tam(), ogre()]), json!([])), &[]), ["t"]);
        assert_eq!(
            ids(&dark(json!([tam(), ogre()]), json!([])), &[("s1", 60.0)]),
            ["t", "g"]
        );
        let lamp = json!([{ "id": "l", "x": 425, "y": 225, "bright": 10, "dim": 20 }]);
        assert_eq!(ids(&dark(json!([tam(), ogre()]), lamp), &[]), ["t", "g"]);
        // A monster's own torch doesn't give it away in the dark.
        let mut torch = ogre();
        torch["light"] = json!({ "bright": 20, "dim": 40 });
        assert_eq!(ids(&dark(json!([tam(), torch]), json!([])), &[]), ["t"]);
        // Their own token always shows, even one the DM blinded, so they can still move it.
        let mut blind = tam();
        blind["vision"] = json!({ "range": 0, "falloff": 0, "off": true });
        assert_eq!(ids(&dark(json!([blind, ogre()]), json!([])), &[]), ["t"]);
    }
}
