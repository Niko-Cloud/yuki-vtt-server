//! Things attached to a map picture on a board: walls, lights, painted fog and the rest built
//! for that map. They move, turn and resize with it, and go when it goes (the website's
//! `packages/engine/src/attach.ts`). Tokens never attach to a map.

use crate::boards::{MapItem, item_box};
use crate::maps::{Drawing, FogArea, Light, Note, Prop, TableMap, Wall};
use crate::passages::Passage;
use serde::{Deserialize, Serialize};

/// Where a picture's middle is on the board, which way it turns, and its own sides.
struct Frame {
    cx: f64,
    cy: f64,
    cos: f64,
    sin: f64,
    width: f64,
    height: f64,
}

fn frame(item: &MapItem) -> Frame {
    let (across, down) = item_box(item);
    let turn = f64::from(item.rotation.unwrap_or(0)).to_radians();
    Frame {
        cx: item.x + across / 2.0,
        cy: item.y + down / 2.0,
        cos: turn.cos().round(),
        sin: turn.sin().round(),
        width: item.width,
        height: item.height,
    }
}

fn round(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// How a point on a map picture moves when the picture does: kept at the same spot on the
/// picture, through moves, quarter turns and resizes.
pub fn follow(before: &MapItem, after: &MapItem) -> impl Fn([f64; 2]) -> [f64; 2] {
    let (from, to) = (frame(before), frame(after));
    move |[x, y]| {
        let (dx, dy) = (x - from.cx, y - from.cy);
        let u = (dx * from.cos + dy * from.sin) / from.width;
        let v = (-dx * from.sin + dy * from.cos) / from.height;
        let (lx, ly) = (u * to.width, v * to.height);
        [
            round((to.cx + lx * to.cos - ly * to.sin).max(0.0)),
            round((to.cy + lx * to.sin + ly * to.cos).max(0.0)),
        ]
    }
}

fn on(attached: &Option<String>, id: &str) -> bool {
    attached.as_deref() == Some(id)
}

/// Everything attached to a map picture follows it from where it was to where it is now.
pub fn carry(map: &mut TableMap, before: &MapItem, after: &MapItem) {
    let same = before.x == after.x
        && before.y == after.y
        && before.width == after.width
        && before.height == after.height
        && before.rotation.unwrap_or(0) == after.rotation.unwrap_or(0);
    if same {
        return;
    }
    let id = before.id.as_str();
    let follow = follow(before, after);
    let turn = (after.rotation.unwrap_or(0) + 360 - before.rotation.unwrap_or(0)) % 360;
    let (sx, sy) = (after.width / before.width, after.height / before.height);
    let size = f64::from(map.grid.size);
    let (ox, oy) = (f64::from(map.grid.offset_x), f64::from(map.grid.offset_y));
    for wall in map.walls.iter_mut().filter(|w| on(&w.attached_to, id)) {
        wall.a = follow(wall.a);
        wall.b = follow(wall.b);
    }
    for light in map.lights.iter_mut().filter(|l| on(&l.attached_to, id)) {
        [light.x, light.y] = follow([light.x, light.y]);
        if let Some(angle) = light.angle.as_mut() {
            *angle = (*angle + f64::from(turn)) % 360.0;
        }
    }
    for note in map.notes.iter_mut().filter(|n| on(&n.attached_to, id)) {
        [note.x, note.y] = follow([note.x, note.y]);
    }
    for passage in map.passages.iter_mut().filter(|p| on(&p.attached_to, id)) {
        [passage.x, passage.y] = follow([passage.x, passage.y]);
    }
    for area in map.fog_areas.iter_mut().filter(|f| on(&f.attached_to, id)) {
        for point in &mut area.points {
            *point = follow(*point);
        }
    }
    for drawing in map.drawings.iter_mut().filter(|d| on(&d.attached_to, id)) {
        for point in &mut drawing.points {
            *point = follow(*point);
        }
    }
    let cells = |n: f64| ((n * 100.0).round() / 100.0).clamp(0.5, 40.0);
    for prop in map.props.iter_mut().filter(|p| on(&p.attached_to, id)) {
        // Its middle follows; it grows with the picture and turns with it.
        let [mx, my] = follow([
            ox + (f64::from(prop.x) + prop.w / 2.0) * size,
            oy + (f64::from(prop.y) + prop.h / 2.0) * size,
        ]);
        let sideways = turn == 90 || turn == 270;
        let (w, h) = if sideways {
            (cells(prop.w * sy), cells(prop.h * sx))
        } else {
            (cells(prop.w * sx), cells(prop.h * sy))
        };
        (prop.w, prop.h) = (w, h);
        prop.x = ((mx - ox) / size - w / 2.0).round().max(0.0) as u32;
        prop.y = ((my - oy) / size - h / 2.0).round().max(0.0) as u32;
        if turn != 0 {
            prop.rotation = Some((prop.rotation.unwrap_or(0) + turn) % 360);
        }
    }
}

/// Whether a spot on the board lies on a map picture.
pub fn on_item(item: &MapItem, [x, y]: [f64; 2]) -> bool {
    let (across, down) = item_box(item);
    x >= item.x && x <= item.x + across && y >= item.y && y <= item.y + down
}

fn middle(points: &[[f64; 2]]) -> [f64; 2] {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in points {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]
}

/// A thing on a board, named by its kind and id.
#[derive(Debug, Clone, Deserialize)]
pub struct Named {
    pub kind: String,
    pub id: String,
}

pub const KINDS: [&str; 7] = [
    "wall", "light", "note", "fogArea", "drawing", "prop", "passage",
];

/// Things attached to a map picture (`to`), or taken off whatever they were on (`None`):
/// those `pick` says yes to, given each thing's kind, id, middle and what it is on now.
fn set(
    map: &mut TableMap,
    pick: impl Fn(&str, &str, [f64; 2], Option<&str>) -> bool,
    to: Option<&str>,
) {
    let size = f64::from(map.grid.size);
    let (ox, oy) = (f64::from(map.grid.offset_x), f64::from(map.grid.offset_y));
    let put = |kind: &str, id: &str, at: [f64; 2], attached: &mut Option<String>| {
        if pick(kind, id, at, attached.as_deref()) {
            *attached = to.map(str::to_string);
        }
    };
    for w in &mut map.walls {
        put("wall", &w.id, middle(&[w.a, w.b]), &mut w.attached_to);
    }
    for l in &mut map.lights {
        put("light", &l.id, [l.x, l.y], &mut l.attached_to);
    }
    for n in &mut map.notes {
        put("note", &n.id, [n.x, n.y], &mut n.attached_to);
    }
    for p in &mut map.passages {
        put("passage", &p.id, [p.x, p.y], &mut p.attached_to);
    }
    for f in &mut map.fog_areas {
        put("fogArea", &f.id, middle(&f.points), &mut f.attached_to);
    }
    for d in &mut map.drawings {
        put("drawing", &d.id, middle(&d.points), &mut d.attached_to);
    }
    for p in &mut map.props {
        let at = [
            ox + (f64::from(p.x) + p.w / 2.0) * size,
            oy + (f64::from(p.y) + p.h / 2.0) * size,
        ];
        put("prop", &p.id, at, &mut p.attached_to);
    }
}

/// Things attached to a map picture: those named, or (none named) everything whose middle lies
/// on it and isn't attached to another map yet.
pub fn attach(map: &mut TableMap, item: &MapItem, things: Option<&[Named]>) {
    match things {
        Some(named) => set(
            map,
            |kind, id, _, _| named.iter().any(|n| n.kind == kind && n.id == id),
            Some(&item.id),
        ),
        None => set(
            map,
            |_, _, at, now| now.is_none() && on_item(item, at),
            Some(&item.id),
        ),
    }
}

/// Things taken off a map picture: those named, or (none named) all attached to it.
pub fn detach(map: &mut TableMap, item: &str, things: Option<&[Named]>) {
    set(
        map,
        |kind, id, _, now| {
            now == Some(item)
                && things.is_none_or(|named| named.iter().any(|n| n.kind == kind && n.id == id))
        },
        None,
    );
}

/// What is attached to a map picture, kind by kind, as kept to bring back.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub walls: Vec<Wall>,
    pub lights: Vec<Light>,
    pub notes: Vec<Note>,
    pub passages: Vec<Passage>,
    pub fog_areas: Vec<FogArea>,
    pub drawings: Vec<Drawing>,
    pub props: Vec<Prop>,
}

/// Takes what is attached to a map picture off the board, and says what it was.
pub fn take_off(map: &mut TableMap, item: &str) -> Attached {
    fn split<T>(list: &mut Vec<T>, is_on: impl Fn(&T) -> bool) -> Vec<T> {
        let (taken, kept) = std::mem::take(list).into_iter().partition(is_on);
        *list = kept;
        taken
    }
    Attached {
        walls: split(&mut map.walls, |t| on(&t.attached_to, item)),
        lights: split(&mut map.lights, |t| on(&t.attached_to, item)),
        notes: split(&mut map.notes, |t| on(&t.attached_to, item)),
        passages: split(&mut map.passages, |t| on(&t.attached_to, item)),
        fog_areas: split(&mut map.fog_areas, |t| on(&t.attached_to, item)),
        drawings: split(&mut map.drawings, |t| on(&t.attached_to, item)),
        props: split(&mut map.props, |t| on(&t.attached_to, item)),
    }
}

/// Puts things back on the board, attached to a map picture (those not there already).
pub fn put_back(map: &mut TableMap, item: &str, back: Attached) {
    fn add<T>(list: &mut Vec<T>, more: Vec<T>, id: impl Fn(&T) -> &str, attach: impl Fn(&mut T)) {
        for mut thing in more {
            if !list.iter().any(|t| id(t) == id(&thing)) {
                attach(&mut thing);
                list.push(thing);
            }
        }
    }
    let to = || Some(item.to_string());
    add(
        &mut map.walls,
        back.walls,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.lights,
        back.lights,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.notes,
        back.notes,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.passages,
        back.passages,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.fog_areas,
        back.fog_areas,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.drawings,
        back.drawings,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
    add(
        &mut map.props,
        back.props,
        |t| &t.id,
        |t| t.attached_to = to(),
    );
}
