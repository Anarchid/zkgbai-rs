#!/usr/bin/env python3
"""Prepare a DIAGNOSTIC-ONLY source copy for controlled density profiling.

This bypasses normal AI updates. NEVER install its library for gameplay.
Input is one journal `snap` record, not a complete engine save/replay.
Run both revisions with the same snapshot, seed, engine and map. See README.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import io

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--snapshot', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--ref', help='Git baseline; omit for current working sources')
a = p.parse_args()
root = Path(__file__).resolve().parents[2]
out = a.out.resolve()
src = out / 'source'
if src.exists():
    raise SystemExit(f'Refusing to overwrite {src}')
src.mkdir(parents=True)
if a.ref:
    archive = subprocess.check_output(['git', 'archive', a.ref], cwd=root)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(src, filter='data')
else:
    for name in ['Cargo.toml', 'Cargo.lock', 'VERSION']:
        if (root/name).exists(): shutil.copy2(root/name, src/name)
    for name in ['crates', 'tools/terrain-view', 'tools/openness']:
        shutil.copytree(root/name, src/name, ignore=shutil.ignore_patterns('target'))
digest = hashlib.sha256()
for file in sorted((src/'crates').rglob('*.rs')):
    digest.update(str(file.relative_to(src)).encode())
    digest.update(file.read_bytes())
(out/'manifest.json').write_text(json.dumps({
    'base_ref': a.ref or subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
    'working_sources': not bool(a.ref),
    'source_sha256': digest.hexdigest(),
    'snapshot_sha256': hashlib.sha256(a.snapshot.read_bytes()).hexdigest(),
}, indent=2)+'\n')
snap = json.loads(a.snapshot.read_text())
fixture = [[[u[1], u[2], u[3]] for u in snap['units']],
           [[u[1], u[2], u[3]] for u in snap['enemies'] if u[1]]]
(out/'fixture.json').write_text(json.dumps(fixture))
base = src/'crates/zkgbai/src'

def edit(path, old, new):
    f = base/path
    s = f.read_text()
    if old not in s: raise RuntimeError(f'Missing insertion in {path}: {old}')
    f.write_text(s.replace(old, new, 1))

def append(path, s):
    f = base/path
    f.write_text(f.read_text()+s)

edit('ai.rs', '        self.frame = frame;\n',
     '        self.frame = frame;\n        self.density_probe(frame);\n        return; // DIAGNOSTIC LIBRARY ONLY\n')
# The graph's territorial labels are recorded, unlike accumulated buildpower.
append('graph/mod.rs', '''
impl Ai {
    pub(crate) fn density_restore_graph(&mut self) {
        let flags: &[u8] = &FLAGS;
        let g = &mut self.graph;
        g.ally_territory_graphics.clear();
        g.enemy_territory_graphics.clear();
        for (s, &flag) in g.metal_spots.iter_mut().zip(flags) {
            s.owned = flag == 1; s.hostile = flag == 2;
            s.enemy_shadowed = flag == 3; s.ally_shadowed = flag == 4;
            let (x,y) = grid_coords(s.position);
            if s.owned { g.ally_territory_graphics.paint_circle(x,y,25,1); }
            if s.hostile { g.enemy_territory_graphics.paint_circle(x,y,13,1); }
        }
        g.calc_centers();
    }
}
'''.replace('FLAGS', str(snap['spots'])))
append('military/mod.rs', '''
impl Ai {
    pub(crate) fn density_probe(&mut self, frame: i32) {
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            let fixture: Vec<Vec<(String, f32, f32)>> = serde_json::from_str(
                &std::fs::read_to_string(std::env::var("DENSITY_FIXTURE").unwrap()).unwrap()).unwrap();
            for (name, x, z) in &fixture[self.team_id as usize] {
                if let Some(def) = self.defs.get(name) {
                    self.cb.cheat_new_unit(def, Vec3::new(*x, self.cb.map().elevation_at(*x, *z), *z));
                }
            }
        }
        if frame == 90 {
            for u in self.cb.team_units() {
                self.war_unit_idle(u); u.stop(0); u.set_fire_state(0, 0); u.set_move_state(0, 0);
            }
        }
        if self.team_id != 0 || frame < 120 { return; }
        if frame == 120 {
            for u in self.cb.enemy_units() { self.war_enemy_enter_los(u); }
            self.los.update(frame); self.graph_update(frame); self.war_update_targets();
            self.density_restore_graph();
            self.war.frame = 112 + (self.offset + 2) % 14;
            self.war_paint_threat_map();
            eprintln!("DENSITY ready aa={} enemies={} fighters={}", self.war.aas.len(), self.war.targets.len(), self.fighters.len());
            let all = self.war.aas.clone();
            for n in [1usize, 8, 16, 32, 64, all.len()] {
                self.war.aas = all.iter().copied().take(n).collect();
                for rep in 0..5 {
                    let t = std::time::Instant::now();
                    self.war_update_aas();
                    eprintln!("DENSITY aa n={n} rep={rep} ms={:.4}", t.elapsed().as_secs_f64()*1000.0);
                }
            }
            self.war.aas = all;
            self.density_qrf_start();
        }
        let t = std::time::Instant::now();
        // Synthetic elapsed time triggers the normal minute report on the
        // first sync. Subsequent frames allow background work to complete.
        self.precision_update(1800 + frame - 120);
        let ms = t.elapsed().as_secs_f64()*1000.0;
        if ms > 0.1 { eprintln!("DENSITY precision frame={frame} ms={ms:.4}"); }
        if frame % 15 == 0 {
            let t = std::time::Instant::now();
            self.density_qrf_update(frame);
            eprintln!("DENSITY qrf frame={frame} ms={:.4}", t.elapsed().as_secs_f64()*1000.0);
        }
    }
}
''')
# QRF stress test: actual map/assets/own units, 16 explicitly synthetic raid
# groups spread around the map. Do not present this as a replay of a match.
append('military/qrf/mod.rs', '''
impl Ai {
    pub(crate) fn density_qrf_start(&mut self) {
        self.qrf_start(); self.qrf.live = true; self.qrf.algo = "mark".into();
        let units: Vec<_> = self.fighters.values().filter(|f| self.defs.name_of(f.unit) == "cloakraid")
            .take(24).map(|f| f.unit).collect();
        for u in units { self.qrf_add(u); }
        self.qrf.next_team = 100;
        self.qrf_assets();
        let w = self.cb.map().width() as f32 * 8.0;
        let h = self.cb.map().height() as f32 * 8.0;
        self.qrf.groups = (0..16).map(|i| RaidGroup {
            members: vec![100000+i], center: Vec3::xz(w*(0.2+0.2*(i%4) as f32), h*(0.2+0.2*(i/4) as f32)),
            vel: Vec3::ZERO, speed: 100.0, power: 200.0, mobility: Mobility::BOT, pressed: 0, pressed_on: vec![],
        }).collect();
        eprintln!("DENSITY qrf_fixture teams={} groups={} assets={}", self.qrf_teams().len(), self.qrf.groups.len(), self.qrf.assets.len());
    }
    pub(crate) fn density_qrf_update(&mut self, frame: i32) { self.qrf_mark_update(frame); }
}
''')
# Export background compute costs separately, not as simulation-thread time.
worker = base/'kgbutil/worker.rs'
if worker.exists():
    edit('kgbutil/worker.rs', '                if done.send((result, start.elapsed())).is_err()',
         '                eprintln!("DENSITY worker name={} ms={:.4}", std::thread::current().name().unwrap_or("?"), start.elapsed().as_secs_f64()*1000.0);\n                if done.send((result, start.elapsed())).is_err()')
    edit('military/qrf/mod.rs', 'pub(crate) fn density_qrf_update(&mut self, frame: i32) { self.qrf_mark_update(frame); }',
         'pub(crate) fn density_qrf_update(&mut self, frame: i32) { self.qrf.plan_pending = false; self.qrf_mark_update(frame); eprintln!("DENSITY qrf_pending frame={frame} pending={}", self.qrf.plan_pending); }')
# Stress the confirmed expensive fallback using the slowest actual worker
# build-site candidate in this reconstructed base. Count engine queries and
# selected target positions to verify batching, separately from natural AA.
append('graph/mod.rs', r'''
thread_local! {
    static DENSITY_FORCED_SITE: std::cell::Cell<Option<Vec3>> = const { std::cell::Cell::new(None) };
    static DENSITY_SITE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(crate) fn density_site_calls() -> usize { DENSITY_SITE_CALLS.with(|n| n.replace(0)) }
impl Ai {
    pub(crate) fn density_force_site(&self, enabled: bool) {
        let mut worst = (0.0f64, None);
        if enabled {
            for w in self.eco.workers.values() {
                let pos = get_directional_point(w.get_pos(), self.graph.ally_center, 100.0);
                let t = std::time::Instant::now();
                for _ in 0..3 { self.graph_build_site(pos); }
                if t.elapsed().as_secs_f64() > worst.0 { worst = (t.elapsed().as_secs_f64(), Some(pos)); }
            }
            eprintln!("DENSITY forced_site pos={:?} ms={:.4}", worst.1, worst.0 * 1000.0 / 3.0);
        }
        DENSITY_FORCED_SITE.with(|p| p.set(worst.1));
        density_site_calls();
    }
}
''')
edit('graph/mod.rs', '    fn graph_build_site(&self, pos: Vec3) -> Vec3 {',
     '    fn graph_build_site(&self, pos: Vec3) -> Vec3 {\n        DENSITY_SITE_CALLS.with(|n| n.set(n.get()+1));')
edit('graph/mod.rs', '    pub fn graph_get_closest_air_haven(&self, position: Vec3) -> Vec3 {',
     '    pub fn graph_get_closest_air_haven(&self, position: Vec3) -> Vec3 {\n        if let Some(pos) = DENSITY_FORCED_SITE.with(|p| p.get()) { return self.graph_build_site(pos); }')
edit('military/mod.rs', '            self.war.aas = all;\n            self.density_qrf_start();', r'''
            self.war.aas = all.clone();
            self.density_force_site(true);
            for n in [1usize, 8, 16, 32, 64, all.len()] {
                self.war.aas = all.iter().copied().take(n).collect();
                for rep in 0..5 {
                    crate::graph::density_site_calls();
                    self.war.target_markers.clear();
                    let t = std::time::Instant::now();
                    self.war_update_aas();
                    let ms = t.elapsed().as_secs_f64()*1000.0;
                    let calls = crate::graph::density_site_calls();
                    let positions: Vec<_> = self.war.target_markers.iter().map(|m| (m.position.x, m.position.z)).collect();
                    eprintln!("DENSITY aa_stress n={n} rep={rep} ms={ms:.4} queries={calls} targets={positions:?}");
                }
            }
            self.war.aas = all;
            self.density_force_site(false);
            self.density_qrf_start();
''')
print(f'Diagnostic source: {src}\nFixture: {out / "fixture.json"}')
