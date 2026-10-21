//! Start-of-game terrain checks: which factories make sense on this map.

use std::collections::BTreeSet;

use springai::UnitDef;

use crate::Ai;

const TA_MSG: &str = "Terrain Analysis: ";

/// Outcome of one movement class's pathability check.
struct PathResult {
    result: bool,
    /// Mean of path cost / straight-line length over the reachable links.
    avg_cost_ratio: f32,
}

pub struct TerrainAnalyzer {
    initial_fac_list: BTreeSet<String>,
    spiders_viable: bool,
    bots_viable: bool,
    ships_viable: bool,
}

impl TerrainAnalyzer {
    /// Run the analysis. Needs the graph (metal spots and links) and the
    /// pathfinder to be initialised.
    pub fn new(ai: &mut Ai) -> Self {
        let mut ta = TerrainAnalyzer {
            initial_fac_list: BTreeSet::new(),
            spiders_viable: false,
            bots_viable: false,
            ships_viable: false,
        };
        ta.populate_fac_list(ai);
        ta
    }

    /// Factory unit-def names viable on this map.
    pub fn get_initial_fac_list(&self) -> &BTreeSet<String> {
        &self.initial_fac_list
    }

    pub fn are_spiders_viable(&self) -> bool {
        self.spiders_viable
    }

    pub fn are_bots_viable(&self) -> bool {
        self.bots_viable
    }

    pub fn are_ships_viable(&self) -> bool {
        self.ships_viable
    }

    fn add(&mut self, fac: &str) {
        self.initial_fac_list.insert(fac.to_owned());
    }

    fn populate_fac_list(&mut self, ai: &mut Ai) {
        // TODO: re-tweak for current ZK version..
        let veh_path = ai.defs.get("vehcon");
        let bot_path = ai.defs.get("cloakcon");
        let spider_path = ai.defs.get("spidercon");
        let hover_path = ai.defs.get("hovercon");
        let amph_path = ai.defs.get("amphcon");
        let boat_path = ai.defs.get("shipcon");

        ai.log(&format!("{TA_MSG}Checking Veh Pathability.."));
        let veh = check_pathing(ai, veh_path, 1.3);
        if veh.result {
            ai.log(&format!("{TA_MSG}Veh path check succeeded, enabling veh!"));
            self.add("factoryveh");
            if veh.avg_cost_ratio < 1.2 {
                self.add("factorytank"); // fuck tanks. Kodachi nerf and dart buff have made them unplayable.
            }
            self.add("factoryhover");
        }

        if !veh.result {
            ai.log(&format!("{TA_MSG}Checking Hover Pathability.."));
            let hover = check_pathing(ai, hover_path, 1.3);
            if hover.result {
                ai.log(&format!("{TA_MSG}Hover path check succeeded, enabling hovers!"));
                self.add("factoryhover");
            }
        }

        ai.log(&format!("{TA_MSG}Checking Bot Pathability.."));
        let bot = check_pathing(ai, bot_path, 1.35);
        if bot.result
        /* && (!veh.result || bot.avg_cost_ratio < veh.avg_cost_ratio - 0.05)
        || ai.merged_allies > 3 */
        {
            ai.log(&format!("{TA_MSG}Bot path check succeeded, enabling bots!"));
            self.add("factorycloak");
            self.add("factoryshield");
            self.add("factoryamph");
            self.add("factoryjump");
        } else if veh.result && bot.avg_cost_ratio >= veh.avg_cost_ratio - 0.05 {
            ai.log(&format!("{TA_MSG}Bots not cost competitive, skipping!"));
        }
        self.bots_viable = bot.result;

        ai.log(&format!("{TA_MSG}Checking Spider Pathability.."));
        let spider = check_pathing(ai, spider_path, 1.35);
        if spider.result
            && (((!bot.result || spider.avg_cost_ratio < bot.avg_cost_ratio - 0.02)
                && (!veh.result || spider.avg_cost_ratio < veh.avg_cost_ratio - 0.05))
                || ai.merged_allies > 7)
        {
            ai.log(&format!("{TA_MSG}Spider path check succeeded, enabling spiders and jumps!"));
            self.add("factoryspider");
            //self.add("factoryjump");
        } else if spider.avg_cost_ratio >= bot.avg_cost_ratio - 0.02
            || spider.avg_cost_ratio >= veh.avg_cost_ratio - 0.05
        {
            ai.log(&format!("{TA_MSG}Spiders not cost competitive, skipping!"));
        }
        self.spiders_viable = spider.result;

        ai.log(&format!("{TA_MSG}Checking Boat Pathability.."));
        let boat = check_pathing(ai, boat_path, 5.0);
        if boat.result {
            ai.log(&format!("{TA_MSG}Boat path check succeeded, enabling boats!"));
            self.add("factoryship");
        }
        self.ships_viable = boat.result;

        if !bot.result && !veh.result {
            ai.log(&format!("{TA_MSG}Checking Amph Pathability.."));
            let amph = check_pathing(ai, amph_path, 5.0);
            if amph.result {
                ai.log(&format!("{TA_MSG}Amph path check succeeded, enabling amphs!"));
                self.add("factoryamph");
            }
        }

        if ai.allies.len() > 2 {
            ai.log(&format!("{TA_MSG}Allies detected, enabling gs start!"));
            self.add("factorygunship");
        }

        let team_size = ai.merged_allies + 1;
        let too_few = |list: &BTreeSet<String>| (list.len() as i64) < team_size as i64;
        if self.initial_fac_list.len() < 3 || too_few(&self.initial_fac_list) {
            ai.log(&format!("{TA_MSG}Terrain Analysis Failed (or team was too big)! Enabling random factories."));
            for fac in ["factorycloak", "factoryshield", "factoryamph", "factoryveh", "factorytank", "factoryhover"] {
                self.add(fac);
            }
            if too_few(&self.initial_fac_list) {
                self.add("factoryplane");
            }
            if too_few(&self.initial_fac_list) {
                self.add("factoryspider");
                self.add("factoryjump");
            }
        }
    }
}

/// Path every graph link with `path_type`'s movement class and compare the
/// path costs to the straight-line lengths.
fn check_pathing(ai: &mut Ai, path_type: Option<UnitDef>, max_rel_cost: f32) -> PathResult {
    let Some(path_type) = path_type else {
        // port: the C++ dereferenced the missing def.
        ai.log(&format!("{TA_MSG}Path Check Failed: unknown constructor def."));
        return PathResult { result: false, avg_cost_ratio: f32::MAX };
    };

    let mut avg_rel_cost = 0.0f32;
    let mut success = true;
    let mut fail = 0.0f32;
    let links = ai.graph.get_links();
    for l in links {
        let spots = &ai.graph.metal_spots;
        let (Some(v0), Some(v1)) = (spots.get(l.v0), spots.get(l.v1)) else {
            fail += 1.0;
            continue;
        };
        let (p0, p1) = (v0.get_pos(), v1.get_pos());
        let mut link_cost = ai.pathfinder.get_path_cost(&ai.war, p0, p1, path_type);
        if link_cost < 0.0 {
            link_cost = ai.pathfinder.get_path_cost(&ai.war, p1, p0, path_type);
        }
        if link_cost < 0.0 {
            //success = false;
            fail += 1.0;
        } else {
            // A degenerate (zero-length) link counts at nominal cost.
            avg_rel_cost += if l.length > 0.0 { link_cost / l.length } else { 1.0 };
        }
    }
    let n_links = links.len() as f32;
    // consider >30% unreachable as failed, as maps are diverse
    if fail > n_links * 0.3 {
        success = false;
    }

    // With no reachable links this is 0/0 = NaN, as in the original: every
    // comparison against the ratio is then false.
    avg_rel_cost /= n_links - fail;
    if !success {
        ai.log(&format!("{TA_MSG}Path Check Failed: too many unreachable mexes."));
    }
    ai.log(&format!("{TA_MSG}Average Relative Path Cost: {avg_rel_cost:.6}"));
    if success && avg_rel_cost > max_rel_cost {
        ai.log(&format!("{TA_MSG}Path Check Failed: high path costs."));
        success = false;
    }
    PathResult { result: success, avg_cost_ratio: avg_rel_cost }
}
