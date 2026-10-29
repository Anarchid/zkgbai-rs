#!/usr/bin/env python3
"""Estimate pack vs assault conditions from archived, paired save experiments.

Primary comparison is the pre-existing mixedC arm vs plain from p6/run2.
Features use only the harvest journal at/before the save, never later enemy
factories, realised kills, or treatment-arm positions. Replicates average
within a scenario; uncertainty clusters by parent harvest game. This is an
exploratory estimate of the old tactics, not validation of a switching policy.

Usage: python3 pack_situations.py --runs ~/Programs/zkgbai-runs --out DIR
Requires numpy, scipy and matplotlib. Writes CSV/JSON and a labelled figure.
"""
import argparse
import csv
import hashlib
import json
import math
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
from scipy.stats import t as student_t

RAIDERS = set('cloakraid shieldraid amphraid hoverraid vehraid cloakheavyraid tankheavyraid jumpraid spiderscout vehscout shieldscout tankraid'.split())
COUNTERS = set('cloakriot cloakassault shieldriot shieldfelon vehriot hoverriot spiderriot spideremp tankriot amphriot'.split())
# Six operational hypotheses. All subgroup reads are exploratory, not preregistered.
RULES = [
    ('free_value', 'Lightly defended nearby eco ≥ 250 metal', 'free_value', 250.0, 'ge'),
    ('healthy', 'Pack health ≥ 85%', 'health', .85, 'ge'),
    ('cohesive', 'At least 80% within 400 elmos', 'cohesion', .8, 'ge'),
    ('light_pressure', 'Nearby mobile metal ≤ half the pack', 'mobile_ratio', .5, 'le'),
    ('few_counters', 'Nearby riot/EMP metal ≤ quarter pack', 'counter_ratio', .25, 'le'),
    ('spare_force', 'Pack ≤ half our ground army metal', 'army_share', .5, 'le'),
]


def read_csv(path, delim=','):
    with path.open() as f:
        return list(csv.DictReader(f, delimiter=delim))


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def stats(rows, key):
    """Scenario-weighted mean and small-sample game-cluster robust interval."""
    if not rows:
        return None
    vals = np.array([r[key] for r in rows], float)
    mu = float(vals.mean())
    groups = sorted({r['game'] for r in rows})
    residual_sums = [sum(r[key] - mu for r in rows if r['game'] == g) for g in groups]
    se = math.sqrt(len(groups) / (len(groups) - 1) * sum(v*v for v in residual_sums)) / len(rows) if len(groups) > 1 else None
    crit = float(student_t.ppf(.975, len(groups)-1)) if len(groups) > 1 else None
    return dict(mean=mu, se=se, lo=mu-crit*se if se is not None else None,
                hi=mu+crit*se if se is not None else None,
                scenarios=len(rows), games=len(groups),
                scenario_se=float(vals.std(ddof=1)/math.sqrt(len(vals))) if len(vals)>1 else None)


def features(meta, snap, costs):
    units = snap['units']
    ravs = [u for u in units if u[1]=='vehassault' and u[5]!='retreating']
    if not ravs:
        raise ValueError(f"{meta['scen']}: no available Ravagers at save")
    squads = Counter(u[6] for u in ravs if u[6] is not None)
    sid = squads.most_common(1)[0][0] if squads else None
    cores = [u for u in ravs if u[6]==sid] if sid is not None else ravs
    pos = tuple(float(np.median([u[i] for u in cores])) for i in (2,3))
    health = float(np.mean([u[4]/100 for u in cores]))
    metal = sum(costs.get(u[1],250)*u[4]/100 for u in cores)
    denom = max(1.,metal)
    enemies = [e for e in snap['enemies'] if e[5]&512 and not e[5]&256 and (e[5]&4 or e[4]<=900)]
    combat = [e for e in enemies if e[7]>0 and not e[5]&(64|128)]
    mobile = [e for e in combat if not e[5]&4 and dist(pos,e[2:4])<=900]
    static = [e for e in combat if e[5]&8 and dist(pos,e[2:4])<=1500]
    eco = [e for e in enemies if e[5]&4 and not e[5]&8 and e[7]==0 and dist(pos,e[2:4])<=1500]
    free = [e for e in eco if sum(b[6] for b in combat if dist(e[2:4], b[2:4])<=b[7]+150) <= .25*denom]
    # Ground combat units already in a squad or a combat/retreat role; not workers/buildings.
    ground = [u for u in units if u[5] in ('squad','raider_squad','raider_solo','raider','qrf','qrf_moonlight','strider','artillery','loner','fighter','aa','support','retreating') and not u[1].startswith(('plane','gunship','bomber'))]
    army = sum(costs.get(u[1],0) for u in ground)
    return dict(scen=meta['scen'], game=meta['dir'], map=meta['map'], save_f=int(meta['save_f']),
                snapshot_f=snap['f'], count=len(cores), health=health, pack_metal=metal,
                cohesion=sum(dist(pos,u[2:4])<=400 for u in cores)/len(cores),
                free_value=sum(e[6] for e in free), local_eco=sum(e[6] for e in eco),
                mobile_ratio=sum(e[6] for e in mobile)/denom,
                counter_ratio=sum(e[6] for e in mobile if e[1] in COUNTERS)/denom,
                raider_ratio=sum(e[6] for e in mobile if e[1] in RAIDERS)/denom,
                porc_ratio=sum(e[6] for e in static)/denom,
                army_share=sum(costs.get(u[1],250) for u in cores)/max(1,army),
                defend_weight=sum(x[2] for x in snap.get('defend',[])),
                x=pos[0],z=pos[1])


def passes(row, rule):
    _,_,key,threshold,op=rule
    return row[key]>=threshold if op=='ge' else row[key]<=threshold


def select_rule(train):
    # Fit only one binary split; require support in both branches and >=2 games
    # on the selected side. Include constant policies, so no forced switch.
    candidates=[('assault',lambda r:False),('pack',lambda r:True)]
    for rule in RULES:
        for inverse in (False,True):
            gate=lambda r,rule=rule,inverse=inverse: passes(r,rule)!=inverse
            yes=[r for r in train if gate(r)]
            no=[r for r in train if not gate(r)]
            if min(len(yes),len(no))>=6 and len({r['game'] for r in yes})>=2:
                candidates.append((rule[0]+(' inverse' if inverse else ''),gate))
    ranked=[]
    for name,gate in candidates:
        value=np.mean([r['delta_weighted'] if gate(r) else 0. for r in train])
        team=np.mean([r['delta_team'] if gate(r) else 0. for r in train])
        # Reject policies whose training benefit comes with team-level metal loss.
        if team>=0:
            ranked.append((value,name,gate))
    return max(ranked,key=lambda x:x[0])[1:]


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--runs',type=Path,default=Path.home()/'Programs/zkgbai-runs')
    ap.add_argument('--out',type=Path,required=True)
    args=ap.parse_args();args.out.mkdir(parents=True,exist_ok=True)
    root=args.runs/'pack/scen';path=root/'score2.csv'
    raw=read_csv(path); metas=read_csv(root/'scenarios.tsv','\t')
    costs={}
    for l in (args.runs/'loop/scorers/zk-costs.tsv').read_text().splitlines():
        p=l.split()
        if len(p)>=2: costs[p[0]]=float(p[1])
    # Audit source totals: terraunit is a temporary helper with a nominal
    # 100,000-metal cost; spawned mines/drones also do not spend that cost.
    corrected = args.out/'corrected-runs.csv'
    audit = Counter()
    for r in raw:
        vals = dict.fromkeys(('eco','porc','mob','lost_m','all+','all-'),0.)
        log = root/'runs-rep4-p6'/r['scen']/r['run']/'load/combatlog.tsv'
        if not log.exists(): raise FileNotFoundError(log)
        for line in log.open(errors='replace'):
            if not line.startswith('kill\t'): continue
            a=line.rstrip('\n').split('\t')
            if len(a)<10: continue
            start=int(r['scen'].rsplit('-',1)[1])
            if not start <= int(a[1]) < start+4500: continue
            name,team,attacker,ateam=a[3],a[4],a[8],a[9]
            if name in ('terraunit','wolverine_mine') or name.startswith('drone'):
                audit[name]+=1
                continue
            value=costs.get(name,0.)
            if team=='0':
                vals['all-']+=value
                if name=='vehassault': vals['lost_m']+=value
            elif team=='1' and ateam=='0':
                vals['all+']+=value
                if attacker=='vehassault':
                    kind='mob'
                    if name.startswith(('static','energy','factory','turret','plate','striderhub','hub')):
                        kind='porc' if name.startswith('turret') or name in ('staticarty','staticheavyarty','staticshield','staticantinuke','staticjammer') else 'eco'
                    vals[kind]+=value
        r.update(vals)
        r['net']=vals['eco']+vals['porc']+vals['mob']-vals['lost_m']
        r['lost']=vals['lost_m']/costs['vehassault']
    with corrected.open('w',newline='') as f:
        writer=csv.DictWriter(f,fieldnames=list(raw[0]));writer.writeheader();writer.writerows(raw)
    # Store only snapshots requested by the fixed scenario manifest.
    snapshots={}
    for game in sorted({m['dir'] for m in metas}):
        requests=sorted((int(m['save_f']),m['scen']) for m in metas if m['dir']==game)
        snaps=[]
        for journal in sorted((root/game/'journal').glob('*.jsonl')):
            for l in journal.open():
                d=json.loads(l)
                if d.get('k')=='snap':snaps.append(d)
        snaps.sort(key=lambda s:s['f'])
        for fr,sc in requests:
            snap=next(s for s in reversed(snaps) if s['f']<=fr)
            if fr-snap['f']>150: raise ValueError(f'{sc}: stale snapshot')
            snapshots[sc]=snap
    feat={m['scen']:features(m,snapshots[m['scen']],costs) for m in metas}
    cells=defaultdict(list)
    for r in raw:cells[r['scen'],r['arm']].append(r)
    outcomes={}
    for (sc,arm),rs in cells.items():
        d={key:float(np.mean([float(r.get(key) or 0) for r in rs])) for key in ('eco','porc','mob','lost_m','all+','all-')}
        d.update(weighted=1.5*(d['eco']+d['porc'])+d['mob']-d['lost_m'],raw=d['eco']+d['porc']+d['mob']-d['lost_m'],team=d['all+']-d['all-'],replicates=len(rs))
        outcomes[sc,arm]=d
    contrasts=[]
    for sc,f in feat.items():
        for arm in ('pass','passC','charge','mixedC','ram'):
            a=outcomes[sc,arm];b=outcomes[sc,'plain']
            contrasts.append(dict(f,arm=arm,replicates=a['replicates'],baseline_replicates=b['replicates'],
                **{'delta_'+k:a[k]-b[k] for k in ('weighted','raw','team','eco','porc','mob','lost_m')}))
    primary=[r for r in contrasts if r['arm']=='mixedC']
    summary={arm:{metric:stats([r for r in contrasts if r['arm']==arm],metric) for metric in ('delta_weighted','delta_raw','delta_team','delta_eco','delta_lost_m')} for arm in ('pass','passC','charge','mixedC','ram')}
    strata=[]
    for rule in RULES:
        for on in (True,False):
            rows=[r for r in primary if passes(r,rule)==on]
            if rows:strata.append(dict(rule=rule[0],label=rule[1],on=on,
                weighted=stats(rows,'delta_weighted'),team=stats(rows,'delta_team'),raw=stats(rows,'delta_raw')))
    folds=[];policy=[]
    for game in sorted({r['game'] for r in primary}):
        train=[r for r in primary if r['game']!=game];test=[r for r in primary if r['game']==game]
        name,gate=select_rule(train)
        for r in test:policy.append(dict(r,policy_weighted=r['delta_weighted'] if gate(r) else 0.,policy_team=r['delta_team'] if gate(r) else 0.,choose_pack=gate(r),policy_vs_pack=(r['delta_weighted'] if gate(r) else 0.)-r['delta_weighted']))
        folds.append(dict(held_out=game,rule=name,scenarios=len(test),pack=sum(gate(r) for r in test),weighted=float(np.mean([r['delta_weighted'] if gate(r) else 0. for r in test]))))
    # An oracle is only a descriptive ceiling on these noisy realised means.
    oracle=[dict(r,oracle=max(0.,r['delta_weighted'])) for r in primary]
    report=dict(source=str(path),excluded_events=dict(audit),sha256=hashlib.sha256(path.read_bytes()).hexdigest(),runs=len(raw),scenarios=len(primary),games=len(folds),primary_runs=sum(len(v) for (sc,arm),v in cells.items() if arm in ('plain','mixedC')),
        metrics={'weighted':'Ravager credited kills: 1.5 × (economy + turrets) + mobiles − Ravager losses, metal / 150 s',
                 'team':'All team credited enemy kills − all own unit losses, metal / 150 s'},
        summary=summary,strata=strata,folds=folds,by_game=[dict(game=g,weighted=stats([r for r in primary if r['game']==g],'delta_weighted'),team=stats([r for r in primary if r['game']==g],'delta_team')) for g in sorted({r['game'] for r in primary})],
        policy=dict(weighted=stats(policy,'policy_weighted'),team=stats(policy,'policy_team'),vs_always_pack=stats(policy,'policy_vs_pack'),pack_scenarios=sum(r['choose_pack'] for r in policy)),
        oracle=stats(oracle,'oracle'),
        limitations=['Only five parent games; overlapping saves and repeated loads are not independent games.',
          'Harvest selected saves before packs engaged; missing idle/defensive/disastrous regimes, including h3 with no qualifying save.',
          'Features describe the save, 5–35 s before the harvest pack engagement, rather than the later treatment-specific decision.',
          'Old p6 tactics: no local sweep, no later repair/porc changes. Later sweep-only runs do not identify sweep vs assault.',
          'Team totals recomputed excluding terraunit, spawned Wolverine mines and drones; original whole-army CSV totals are corrupted by nominal helper costs.',
          'Last-hit credit and 150-second horizons miss shared damage, economic denial and long-run defense costs.',
          'Opponent factory family is confounded with parent game; not a deployable decision feature.',
          'Intervals use game-cluster robust SE and Student t with G−1 df; subgroup intervals and any learned rule are exploratory.',
          'Held-out-game policy chooses a single fixed operational split, with >=6 training scenarios per branch and no negative mean team trade in training.'])
    (args.out/'estimate.json').write_text(json.dumps(report,indent=2)+'\n')
    for name,rows in [('scenarios.csv',contrasts),('held-out-policy.csv',policy)]:
        with (args.out/name).open('w',newline='') as f:
            w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    plt.rcParams.update({'font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    fig,axs=plt.subplots(1,2,figsize=(13,5.8),gridspec_kw={'width_ratios':[1,1.6]})
    arms=['pass','passC','charge','mixedC','ram']; labels=['Tangential pass','Committed pass','Charge','Mixed (current)','Ram; no turret cap']
    for i,arm in enumerate(arms):
        d=summary[arm]['delta_weighted'];axs[0].errorbar(d['mean'],i,xerr=[[d['mean']-d['lo']],[d['hi']-d['mean']]],fmt='o',color='#167d8d' if arm=='mixedC' else '#6a747b',capsize=3)
    axs[0].set_yticks(range(5),labels);axs[0].invert_yaxis();axs[0].axvline(0,color='#999999',lw=1)
    axs[0].set_xlabel('Extra weighted Ravager net vs assault (metal / 150 s)');axs[0].set_ylabel('Pack tactic');axs[0].set_title('Average tactic effect; 95% game-cluster intervals')
    display=[s for s in strata if s['on'] and s['weighted']['games']>1]
    for i,s in enumerate(display):
        d=s['weighted'];axs[1].errorbar(d['mean'],i,xerr=[[d['mean']-d['lo']],[d['hi']-d['mean']]],fmt='o',color='#167d8d',capsize=3)
    axs[1].set_yticks(range(len(display)),[s['label']+f" ({s['weighted']['scenarios']} saves / {s['weighted']['games']} games)" for s in display]);axs[1].invert_yaxis();axs[1].axvline(0,color='#999999',lw=1)
    axs[1].set_xlabel('Extra weighted Ravager net vs assault (metal / 150 s)');axs[1].set_ylabel('Initial situation; each is a separate subgroup');axs[1].set_title('Mixed drive: pre-save conditions, exploratory')
    fig.suptitle('When do Ravager pack drives beat assault a-moves?',fontsize=15)
    fig.text(.02,.015,'Source: pack/scen p6 run2 · 28 Oct 2026 · 240 primary-comparison loads, 30 saves from 5 games · static kills weighted 1.5×\nNo sweep in this experiment. Intervals cluster overlapping saves by parent game; none establishes a switching rule.',fontsize=9)
    fig.tight_layout(rect=(0,.095,1,.95));fig.savefig(args.out/'pack-tactics.png',dpi=170);fig.savefig(args.out/'pack-tactics.svg');plt.close(fig)
    print(json.dumps({'excluded_events':dict(audit),'counts':{k:report[k] for k in ('runs','scenarios','games')},'mixed':summary['mixedC'],'strata':strata,'policy':report['policy'],'folds':folds},indent=2))


if __name__=='__main__':main()
