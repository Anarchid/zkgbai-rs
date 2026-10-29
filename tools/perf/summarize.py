#!/usr/bin/env python3
"""Summarize density.py arena stdout files as JSON; keep worker and main time separate."""
import argparse
import ast
import json
from pathlib import Path
import re
import statistics

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('logs', nargs='+', type=Path)
args = parser.parse_args()

def summarize(path):
    text = path.read_text()
    def sample(pattern):
        values = [float(x) for x in re.findall(pattern, text)]
        return {'samples_ms': values, 'mean_ms': statistics.mean(values) if values else None,
                'max_ms': max(values, default=None), 'total_ms': sum(values)}
    records = {}
    for kind in ['aa', 'aa_stress']:
        records[kind] = {}
        for count in sorted(set(int(x) for x in re.findall(rf'DENSITY {kind} n=(\d+)', text))):
            record = sample(rf'DENSITY {kind} n={count} rep=\d+ ms=([\d.]+)')
            if kind == 'aa_stress':
                record['build_site_calls'] = [int(x) for x in re.findall(
                    rf'DENSITY {kind} n={count}.*?queries=(\d+)', text)]
                targets = [ast.literal_eval(x) for x in re.findall(
                    rf'DENSITY {kind} n={count}.*?targets=([^\n]*)', text)]
                record['unique_selected_targets'] = sorted(set(p for batch in targets for p in batch))
                record['selected_target_counts'] = [len(batch) for batch in targets]
            records[kind][count] = record
    records['qrf_main'] = sample(r'DENSITY qrf frame=\d+ ms=([\d.]+)')
    # Precision logs omit calls below 0.1 ms. Do not interpret their mean as
    # the mean update cost; the cold maximum is the useful measurement here.
    records['precision_main_above_0_1ms'] = sample(r'DENSITY precision frame=\d+ ms=([\d.]+)')
    for worker in ['qrf-fields', 'precision-shadows']:
        records[worker + '_worker'] = sample(rf'DENSITY worker name={worker} ms=([\d.]+)')
    states = re.findall(r'DENSITY qrf_pending frame=\d+ pending=(true|false)', text)
    records['qrf_plans_pending'] = {'pending': states.count('true'), 'ready': states.count('false')}
    records['forced_site'] = re.findall(r'DENSITY forced_site ([^\n]+)', text)
    return records

print(json.dumps({str(path): summarize(path) for path in args.logs}, indent=2))
