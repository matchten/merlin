//! Pins as limits: when the user pins layers or a time range and locks them, Merlin's edits must
//! stay inside. [`check`] compares the project before and after one tool call and lists what changed
//! outside the [`Scope`]; the panel then undoes that call and tells the model why, so it can redo
//! the edit within bounds.
//!
//! A range is checked by what it looks like, not by where keys sit: every changed property is
//! sampled at the frames outside the range before and after, so a new key whose curve spills past
//! the range, a static value change (visible at every frame) or a moved in point are all caught.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerId, Project, PropGroup, Property};
use effectcraft_time::Tick;

/// Where edits may land.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    /// The comp the pins belong to (item id).
    pub comp: u64,
    /// Only these layers may change (new layers are fine).
    pub layers: Option<BTreeSet<u64>>,
    /// Only frames in this comp-time range may change (inclusive).
    pub range: Option<(Tick, Tick)>,
}

impl Scope {
    pub fn is_empty(&self) -> bool {
        self.layers.is_none() && self.range.is_none()
    }
}

/// Most frames outside the range sampled per changed property.
const MAX_SAMPLES: i64 = 600;

/// What changed outside `scope` between `before` and `after`, one line each (empty: all inside).
/// `when` formats a comp time for the messages.
pub fn check(before: &Project, after: &Project, scope: &Scope, when: &dyn Fn(Tick) -> String) -> Vec<String> {
    let mut out = vec![];
    if scope.is_empty() {
        return out;
    }
    let cid = ItemId(scope.comp);
    // Other compositions are out of bounds (new ones, such as precomps, are fine).
    for (id, item) in &before.items {
        let ItemKind::Comp(b) = &item.kind else { continue };
        if *id == cid {
            continue;
        }
        match after.comp_arc(*id) {
            Some(a) if Arc::ptr_eq(b, &a) || **b == *a => {}
            Some(_) => out.push(format!("changed the composition \"{}\" (only the pinned comp may change)", item.name)),
            None => out.push(format!("removed the composition \"{}\"", item.name)),
        }
    }
    let (Some(b), Some(a)) = (before.comp(cid), after.comp(cid)) else {
        if before.comp(cid).is_some() {
            out.push("removed the pinned composition".into());
        }
        return out;
    };
    if before.comp_arc(cid).zip(after.comp_arc(cid)).is_some_and(|(x, y)| Arc::ptr_eq(&x, &y)) {
        return out;
    }
    if comp_shell(b) != comp_shell(a) {
        out.push("changed the composition's settings (they affect every layer and frame)".into());
    }
    let outside = scope.range.map(|(s, e)| frames_outside(a, s.min(e), s.max(e))).unwrap_or_default();
    let range_text = scope.range.map(|(s, e)| format!("{} to {}", when(s.min(e)), when(s.max(e)))).unwrap_or_default();
    for bl in &b.layers {
        let allowed = scope.layers.as_ref().is_none_or(|set| set.contains(&bl.id.0));
        let Some(al) = a.layer(bl.id) else {
            out.push(format!("removed the layer \"{}\"", bl.name));
            continue;
        };
        if bl == al {
            continue;
        }
        if !allowed {
            out.push(format!("changed the layer \"{}\" (not one of the pinned layers)", bl.name));
            continue;
        }
        if scope.range.is_none() {
            continue;
        }
        if layer_shell(bl) != layer_shell(al) {
            if !same_timing_only(bl, al) {
                out.push(format!("changed the layer \"{}\" itself (switches, blending, parenting or start time affect every frame)", al.name));
                continue;
            }
            // New in and out points: only a problem where they show or hide it outside the range.
            if let Some(t) = outside.iter().find(|t| bl.is_active_at(**t) != al.is_active_at(**t)) {
                out.push(format!("moved the in or out point of \"{}\", which changes {} (outside {range_text})", al.name, when(*t)));
                continue;
            }
        }
        out.extend(prop_changes(bl, al, &outside, when, &range_text));
    }
    // New layers must not show outside the range.
    if scope.range.is_some() {
        let old: BTreeSet<LayerId> = b.layers.iter().map(|l| l.id).collect();
        for l in a.layers.iter().filter(|l| !old.contains(&l.id)) {
            if let Some(t) = outside.iter().find(|t| l.is_active_at(**t)) {
                out.push(format!("the new layer \"{}\" shows at {} outside {range_text} (set its in and out points inside the range)", l.name, when(*t)));
            }
        }
    }
    out
}

/// The comp without its layers and the parts that don't change pixels (markers, work area).
fn comp_shell(c: &Comp) -> Comp {
    Comp { layers: vec![], markers: vec![], work_area: (Tick::ZERO, Tick::ZERO), ..c.clone() }
}

/// The layer without its properties and the parts that don't change pixels (name, comment, label, markers).
fn layer_shell(l: &Layer) -> Layer {
    Layer { props: PropGroup::new(0, "", ""), markers: vec![], name: String::new(), comment: String::new(), label: Default::default(), ..l.clone() }
}

/// Whether two layers differ only in their in and out points (checked frame by frame instead).
fn same_timing_only(a: &Layer, b: &Layer) -> bool {
    layer_shell(&Layer { in_point: b.in_point, out_point: b.out_point, ..a.clone() }) == layer_shell(b)
}

/// The comp's frame times outside `[s, e]`, at most [`MAX_SAMPLES`] spread evenly.
fn frames_outside(c: &Comp, s: Tick, e: Tick) -> Vec<Tick> {
    let fr = c.frame_rate;
    let last = fr.frame_at(c.duration).saturating_sub(1).max(0);
    let (fs, fe) = (fr.frame_at(s), fr.frame_at(e));
    let all: Vec<i64> = (0..=last).filter(|f| *f < fs || *f > fe).collect();
    let step = (all.len() as i64 / MAX_SAMPLES).max(1) as usize;
    all.into_iter().step_by(step).map(|f| fr.tick_of(f)).collect()
}

fn props_by_uid(g: &PropGroup) -> BTreeMap<u64, &Property> {
    let mut m = BTreeMap::new();
    g.walk("", &mut |_, p| {
        m.insert(p.uid, p);
    });
    m
}

/// Changed properties of a layer that look different at a frame outside the range. Properties
/// the edit added (a new effect) are left to the model: the prompt asks it to keep them neutral
/// outside the range.
fn prop_changes(b: &Layer, a: &Layer, outside: &[Tick], when: &dyn Fn(Tick) -> String, range_text: &str) -> Vec<String> {
    let (bp, ap) = (props_by_uid(&b.props), props_by_uid(&a.props));
    let mut out = vec![];
    for (uid, p) in &bp {
        let Some(q) = ap.get(uid) else {
            out.push(format!("removed {}/{}", b.name, p.name));
            continue;
        };
        if p == q {
            continue;
        }
        if p.expr != q.expr && (p.has_expression() || q.has_expression()) {
            out.push(format!("changed the expression on {}/{} (an expression affects every frame)", a.name, q.name));
            continue;
        }
        if let Some(t) = outside.iter().find(|t| !close(&p.value_at(b.layer_time(**t)).to_json(), &q.value_at(a.layer_time(**t)).to_json())) {
            out.push(format!(
                "changed {}/{} at {} (outside {range_text}); to keep it, first add keyframes at the range's ends holding the current values{}",
                a.name,
                q.name,
                when(*t),
                if q.spatial {
                    " and give them zero spatial tangents (setSpatialTangentsAtKey(k, [0,0,0], [0,0,0])) so the path outside keeps its shape"
                } else {
                    ""
                }
            ));
        }
    }
    out
}

/// Equal up to float noise.
fn close(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (a, b) {
        (J::Number(x), J::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            (x - y).abs() <= 1e-4 * x.abs().max(y.abs()).max(1.0)
        }
        (J::Array(x), J::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| close(p, q)),
        (J::Object(x), J::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w))),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_engine::Session;
    use serde_json::json;

    fn session() -> Session {
        let mut s = effectcraft_host::session();
        let r = s.execute(
            "script.run",
            json!({"code": "var c = app.project.items.addComp('Main', 640, 360, 1, 4, 10); c.openInViewer(); var a = c.layers.addSolid([1,0,0], 'A', 100, 100, 1); var b = c.layers.addSolid([0,0,1], 'B', 100, 100, 1); var p = a.property('ADBE Transform Group').property('ADBE Position'); p.setValueAtTime(0, [100, 100]); p.setValueAtTime(3, [400, 100]); var o = a.property('ADBE Transform Group').property('ADBE Opacity'); o.setValueAtTime(0, 0); o.setValueAtTime(3, 100); app.project.items.addComp('Other', 64, 64, 1, 1, 10);"}),
        )
        .unwrap();
        assert!(r["error"].is_null(), "{r}");
        s
    }

    fn ids(s: &Session) -> (u64, u64, u64) {
        let c = s.active_comp().unwrap();
        (s.active_comp_id().unwrap().0, c.layers.iter().find(|l| l.name == "A").unwrap().id.0, c.layers.iter().find(|l| l.name == "B").unwrap().id.0)
    }

    /// Run `code` and check it against `scope`.
    fn after(s: &mut Session, scope: &Scope, code: &str) -> Vec<String> {
        let before = s.project.clone();
        let r =
            s.execute("script.run", json!({"code": format!("var c = app.project.activeItem; var a = c.layer('A'); var b = c.layer('B'); {code}")})).unwrap();
        assert!(r["error"].is_null(), "{code}: {r}");
        let v = check(&before, &s.project, scope, &|t| format!("{:.1}s", t.seconds()));
        s.project = before;
        v
    }

    fn secs(a: f64, b: f64) -> Option<(Tick, Tick)> {
        Some((Tick::from_seconds_f64(a), Tick::from_seconds_f64(b)))
    }

    #[test]
    fn layer_scope() {
        let mut s = session();
        let (comp, a, _) = ids(&s);
        let scope = Scope { comp, layers: Some([a].into()), range: None };
        assert!(after(&mut s, &scope, "a.property('ADBE Transform Group').property('ADBE Rotate Z').setValue(50);").is_empty());
        assert!(after(&mut s, &scope, "c.layers.addSolid([0,1,0], 'New', 10, 10, 1);").is_empty(), "new layers are fine");
        let v = after(&mut s, &scope, "b.property('ADBE Transform Group').property('ADBE Opacity').setValue(50);");
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("\"B\" (not one of the pinned layers)"), "{v:?}");
        assert!(after(&mut s, &scope, "b.remove();")[0].contains("removed the layer \"B\""));
        let v = after(
            &mut s,
            &scope,
            "for (var i = 1; i <= app.project.numItems; i++) { var it = app.project.item(i); if (it.name == 'Other') it.layers.addNull(); }",
        );
        assert!(v[0].contains("\"Other\""), "{v:?}");
        assert!(check(&s.project, &s.project, &Scope::default(), &|_| String::new()).is_empty());
    }

    #[test]
    fn range_scope() {
        let mut s = session();
        let (comp, _, _) = ids(&s);
        let scope = Scope { comp, layers: None, range: secs(1.0, 2.0) };
        let pos = "var p = a.property('ADBE Transform Group').property('ADBE Position');";
        let op = "var o = a.property('ADBE Transform Group').property('ADBE Opacity');";
        // Holding the range's ends first keeps a new middle key inside.
        let v = after(
            &mut s,
            &scope,
            &format!("{op} o.setValueAtTime(1, o.valueAtTime(1, true)); o.setValueAtTime(2, o.valueAtTime(2, true)); o.setValueAtTime(1.5, 100);"),
        );
        assert!(v.is_empty(), "{v:?}");
        assert!(after(&mut s, &scope, &format!("{op} o.setValueAtTime(1.5, 100);"))[0].contains("A/Opacity"));
        // A motion path also needs straight tangents at the holds, else its shape outside bends.
        let hold = format!(
            "{pos} p.setValueAtTime(1, p.valueAtTime(1, true)); p.setValueAtTime(2, p.valueAtTime(2, true)); p.setValueAtTime(1.5, [250, 300]); for (var k = 1; k <= p.numKeys; k++) p.setSpatialTangentsAtKey(k, [0,0,0], [0,0,0]);"
        );
        let v = after(&mut s, &scope, &hold);
        assert!(v.is_empty(), "{v:?}");
        // Without the holds the curve spills out.
        let v = after(&mut s, &scope, &format!("{pos} p.setValueAtTime(1.5, [250, 300]);"));
        assert!(v.len() == 1 && v[0].contains("A/Position") && v[0].contains("outside 1.0s to 2.0s") && v[0].contains("setSpatialTangentsAtKey"), "{v:?}");
        // A static value shows at every frame.
        assert!(after(&mut s, &scope, "b.property('ADBE Transform Group').property('ADBE Opacity').setValue(20);")[0].contains("B/Opacity"));
        assert!(after(&mut s, &scope, "b.property('ADBE Transform Group').property('ADBE Opacity').expression = 'time * 10';")[0].contains("expression"));
        // New layers must stay inside the range.
        let v = after(&mut s, &scope, "c.layers.addSolid([0,1,0], 'Flash', 10, 10, 1);");
        assert!(v[0].contains("new layer \"Flash\""), "{v:?}");
        assert!(after(&mut s, &scope, "var f = c.layers.addSolid([0,1,0], 'Flash', 10, 10, 1); f.inPoint = 1; f.outPoint = 2;").is_empty());
        // Trimming a layer changes the frames it no longer covers.
        assert!(after(&mut s, &scope, "b.outPoint = 3;")[0].contains("in or out point of \"B\""));
        assert!(after(&mut s, &scope, "b.blendingMode = BlendingMode.ADD;")[0].contains("itself"));
    }
}
