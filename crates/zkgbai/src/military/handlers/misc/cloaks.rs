//! Place Iris fields for additional coverage, including taskforce escorts.

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::TAU;

use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::SquadId;
use crate::util::{NULLPOS, distance};

const EDGE: f32 = 60.0; // Leave room for movement and the support's stopping distance.

#[derive(Default)]
pub(super) struct CloakPlanner {
    posts: BTreeMap<i32, Vec3>,
}

#[derive(Clone, Copy)]
struct Field {
    pos: Vec3,
    radius: f32,
}

impl Field {
    fn covers(self, pos: Vec3) -> bool {
        distance(self.pos, pos) <= (self.radius - EDGE).max(0.0)
    }

    /// Fractional penetration of the smaller field's diameter. This is a
    /// spacing penalty, not extra cloak value: complete containment is 1.
    fn overlap(self, other: Self) -> f32 {
        ((self.radius + other.radius - distance(self.pos, other.pos)) / (2.0 * self.radius.min(other.radius)).max(1.0))
            .clamp(0.0, 1.0)
    }
}

struct Group {
    squad: Option<SquadId>,
    priority: u8,
    anchor: Vec3,
    assets: Vec<(Vec3, f32)>,
}

fn positions(at: Vec3, radius: f32) -> impl Iterator<Item = Vec3> {
    std::iter::once(at).chain((0..8).map(move |i| {
        let angle = i as f32 * TAU / 8.0;
        Vec3::xz(at.x + radius * angle.cos(), at.z + radius * angle.sin())
    }))
}

fn coverage(field: Field, assets: &[(Vec3, f32)], others: &[Field]) -> (f32, f32) {
    let mut total = 0.0;
    let mut added = 0.0;
    for &(pos, metal) in assets {
        if field.covers(pos) {
            total += metal;
            if !others.iter().any(|f| f.covers(pos)) {
                added += metal;
            }
        }
    }
    (total, added)
}

/// Reserve each chosen field immediately before choosing the next Iris.
/// Stable ordering plus a modest previous-post bonus prevents swapping.
fn choose(
    groups: &[Group],
    others: &[Field],
    radius: f32,
    from: Vec3,
    previous: Option<Vec3>,
    locked: Option<SquadId>,
    mut allowed: impl FnMut(Vec3, Vec3) -> bool,
) -> Option<(Vec3, Option<SquadId>)> {
    let mut best: Option<(u8, f32, Vec3, Option<SquadId>)> = None;
    for group in groups {
        if locked.is_some() && group.squad != locked {
            continue;
        }
        // Sample members as well as the center: a stretched squad can
        // justify two separate fields. Bound candidate generation per squad.
        let step = group.assets.len().div_ceil(4).max(1);
        let anchors = std::iter::once(group.anchor).chain(group.assets.iter().step_by(step).map(|a| a.0));
        let candidates = previous.into_iter().chain(anchors.flat_map(|at| positions(at, (radius - EDGE) * 0.8)));
        for pos in candidates {
            let field = Field { pos, radius };
            let (total, added) = coverage(field, &group.assets, others);
            // A dedicated battery escort keeps its battery covered even
            // while two batteries pass through one another's cloak fields.
            let value = added + if locked.is_some() { total * 0.1 } else { 0.0 };
            if value <= 0.0 {
                continue;
            }
            let overlap = others.iter().map(|&f| field.overlap(f)).fold(0.0, f32::max);
            let loyalty = if previous.is_some_and(|p| distance(p, pos) < EDGE) { 1.15 } else { 1.0 };
            let score = value * (1.0 - 0.85 * overlap) * loyalty / (1.0 + distance(from, pos) / 2000.0);
            if best.is_some_and(|(priority, old, _, _)| {
                priority < group.priority || (priority == group.priority && old >= score)
            }) || !allowed(pos, group.anchor)
            {
                continue;
            }
            best = Some((group.priority, score, pos, group.squad));
        }
    }
    best.map(|(_, _, pos, squad)| (pos, squad))
}

impl Ai {
    fn cloak_radius(&self, name: &str, fallback: f32) -> f32 {
        self.defs
            .get(name)
            .and_then(|d| d.custom_param("area_cloak_radius"))
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|r| r.is_finite() && *r > EDGE)
            .unwrap_or(fallback)
    }

    pub(super) fn misc_update_cloakers(&mut self, friendly: &[Unit]) {
        let mut owned: Vec<_> = self
            .fighters
            .values()
            .filter_map(|f| {
                if f.is_dead() || self.defs.name_of(f.unit) != "cloakjammer" || self.retreat_handler.is_retreating(f.id)
                {
                    return None;
                }
                let locked = f
                    .squad
                    .filter(|&sid| self.squads.get(sid).is_some_and(|s| s.shape.cloaks(f.unit.def().map(|d| d.id()))));
                (locked.is_some() || self.misc_handler.supports.contains(&f.id)).then_some((f.id, f.unit, locked))
            })
            .collect();
        owned.sort_by_key(|&(id, _, locked)| (locked.is_none(), id));
        let controlled: BTreeSet<_> = owned.iter().map(|u| u.0).collect();
        self.misc_handler.cloaks.posts.retain(|id, _| controlled.contains(id));
        if owned.is_empty() {
            return;
        }
        let radius = self.cloak_radius("cloakjammer", 350.0);
        let static_radius = self.cloak_radius("staticjammer", 400.0);
        let mut fields: Vec<_> = friendly
            .iter()
            .filter_map(|&u| {
                if controlled.contains(&u.id()) || u.health() <= 0.0 || u.is_being_built() {
                    return None;
                }
                let nominal = match self.defs.name_of(u) {
                    "cloakjammer" => radius,
                    "staticjammer" => static_radius,
                    _ => return None,
                };
                if u.rules_param_float("cloak_shield", 2.0) <= 0.0 {
                    return None;
                }
                let r = u.rules_param_float("cloakerRadius", nominal).clamp(0.0, nominal);
                let pos = u.pos();
                // Corneas can shift their cloak field away from the building.
                let pos =
                    Vec3::xz(u.rules_param_float("cloaker_pos_x", pos.x), u.rules_param_float("cloaker_pos_z", pos.z));
                (r > EDGE).then_some(Field { pos, radius: r })
            })
            .collect();
        let groups: Vec<_> = self
            .support_ground_squads()
            .into_iter()
            .filter_map(|sid| {
                let s = self.squads.get(sid)?;
                let assets: Vec<_> = s
                    .everyone()
                    .filter_map(|id| {
                        let f = self.fighters.get(&id)?;
                        if f.is_dead()
                            || self.retreat_handler.is_retreating(id)
                            || self.unit_classes.mob_supports.contains(self.defs.name_of(f.unit))
                            || f.unit.def().is_none_or(|d| d.is_able_to_fly())
                        {
                            return None;
                        }
                        Some((f.get_pos(), f.metal_value))
                    })
                    .collect();
                if assets.is_empty() {
                    return None;
                }
                let n = assets.len() as f32;
                let anchor = Vec3::xz(
                    assets.iter().map(|a| a.0.x).sum::<f32>() / n,
                    assets.iter().map(|a| a.0.z).sum::<f32>() / n,
                );
                Some(Group { squad: Some(sid), priority: 0, anchor, assets })
            })
            .collect();
        let map = self.cb.map();
        let bounds = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        for (id, unit, locked) in owned {
            let Some(def) = unit.def() else { continue };
            let from = unit.pos();
            let previous = self.misc_handler.cloaks.posts.get(&id).copied();
            let allowed = |pos: Vec3, anchor: Vec3| {
                pos.x >= 0.0
                    && pos.z >= 0.0
                    && pos.x < bounds.0
                    && pos.z < bounds.1
                    && self.terrain_reaches(def, from, pos, 0.0)
                    && self.war.get_threat(pos) <= self.war.get_threat(anchor).max(self.war.get_threat(from))
            };
            let mut chosen = choose(&groups, &fields, radius, from, previous, locked, allowed);
            if chosen.is_none() && locked.is_none() {
                let fallback: Vec<_> = self
                    .support_fallbacks(unit, friendly)
                    .into_iter()
                    .map(|(priority, at, target, metal)| Group {
                        squad: None,
                        priority: priority + 1,
                        anchor: target,
                        assets: vec![(at, metal)],
                    })
                    .collect();
                chosen = choose(&fallback, &fields, radius, from, previous, None, allowed);
            }
            let (target, squad) = chosen.unwrap_or_else(|| {
                // Everything useful is already cloaked. Park safely with
                // minimal overlap instead of following another Iris forever.
                let home = self.graph_get_closest_haven(from);
                let home = if home == NULLPOS { from } else { home };
                let target = positions(home, 2.0 * radius)
                    .chain(positions(from, 2.0 * radius))
                    .filter(|&pos| allowed(pos, home))
                    .min_by(|&a, &b| {
                        let score = |pos| {
                            fields.iter().map(|&f| (Field { pos, radius }).overlap(f)).sum::<f32>() * 10000.0
                                + distance(from, pos)
                        };
                        score(a).total_cmp(&score(b))
                    })
                    .unwrap_or(from);
                (target, None)
            });
            if locked.is_none() {
                self.fighters.get_mut(&id).unwrap().squad = squad;
            }
            self.misc_handler.cloaks.posts.insert(id, target);
            fields.push(Field { pos: target, radius });
            self.support_move(unit, target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(squad: u32, x: f32, metal: f32) -> Group {
        Group {
            squad: Some(squad),
            priority: 0,
            anchor: Vec3::xz(x, 1000.0),
            assets: vec![(Vec3::xz(x, 1000.0), metal)],
        }
    }

    #[test]
    fn second_iris_covers_another_squad_instead_of_stacking() {
        let groups = [group(1, 1000.0, 4000.0), group(2, 2000.0, 2000.0)];
        let from = Vec3::xz(1000.0, 1000.0);
        let first = choose(&groups, &[], 350.0, from, None, None, |_, _| true).unwrap();
        assert_eq!(first.1, Some(1));
        let fields = [Field { pos: first.0, radius: 350.0 }];
        let second = choose(&groups, &fields, 350.0, from, Some(first.0), None, |_, _| true).unwrap();
        assert_eq!(second.1, Some(2));
        assert_eq!((Field { pos: second.0, radius: 350.0 }).overlap(fields[0]), 0.0);
    }

    #[test]
    fn adjacent_squads_still_spread_their_fields_and_respect_reachability() {
        let groups = [group(1, 1000.0, 4000.0), group(2, 1400.0, 2000.0)];
        let first = Field { pos: Vec3::xz(1000.0, 1000.0), radius: 350.0 };
        let chosen = choose(&groups, &[first], 350.0, first.pos, None, None, |p, _| p.z >= 1000.0).unwrap();
        let next = Field { pos: chosen.0, radius: 350.0 };
        assert_eq!(chosen.1, Some(2));
        assert!(next.covers(groups[1].anchor));
        assert!(distance(first.pos, next.pos) > 600.0);
        assert!(choose(&groups, &[], 350.0, first.pos, None, None, |_, _| false).is_none());
    }

    #[test]
    fn allied_static_coverage_and_planned_fields_suppress_redundant_assignments() {
        let groups = [group(1, 1000.0, 4000.0)];
        let field = Field { pos: groups[0].anchor, radius: 400.0 };
        assert!(choose(&groups, &[field], 350.0, field.pos, None, None, |_, _| true).is_none());
        // A dedicated escort stays with its battery as the armies cross.
        let locked = choose(&groups, &[field], 350.0, field.pos, None, Some(1), |_, _| true).unwrap();
        assert_eq!(locked.1, Some(1));
        assert!((Field { pos: locked.0, radius: 350.0 }).covers(field.pos));
        assert!(distance(locked.0, field.pos) > 200.0);
    }

    #[test]
    fn stretched_squad_can_use_two_fields_and_empty_groups_use_none() {
        let mut g = group(1, 1000.0, 2000.0);
        g.assets.push((Vec3::xz(1800.0, 1000.0), 2000.0));
        let field = Field { pos: g.anchor, radius: 350.0 };
        let chosen = choose(&[g], &[field], 350.0, field.pos, None, None, |_, _| true).unwrap();
        assert_eq!(chosen.1, Some(1));
        assert!((Field { pos: chosen.0, radius: 350.0 }).covers(Vec3::xz(1800.0, 1000.0)));
        assert!(choose(&[], &[], 350.0, field.pos, None, None, |_, _| true).is_none());
    }
}
