#!/usr/bin/env python3
"""Export and rerun the observational crew-risk protocol in an explicit output directory."""
import argparse
import collections
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path
import sys
import sqlite3
import subprocess

# Keep sibling module bytecode out of the committed workspace.
sys.dont_write_bytecode = True

DAY = 86400
HISTORICAL_CUTOFFS = {
    'initial_review_policy': '2026-09-13T04:25:42+00:00',
    'claimed_filing_fix': '2026-10-08T16:23:38+00:00',
    'typed_attribution': '2026-10-19T00:13:39+00:00',
    'expanded_qa': '2026-10-25T06:59:45+00:00',
}


def timestamp(value):
    parsed = dt.datetime.fromisoformat(value.replace('Z', '+00:00'))
    if parsed.tzinfo is None:
        raise ValueError('timestamps must include a timezone')
    return parsed.timestamp()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')


def read_snapshot(path):
    raw = path.read_bytes()
    if path.suffix == '.gz':
        raw = gzip.decompress(raw)
    value = json.loads(raw)
    if value.get('schema_version') != 1 or not isinstance(value.get('rows'), list) or not isinstance(value.get('links'), list):
        raise ValueError('expected schema_version 1 snapshot with rows and links arrays')
    if len({r['id'] for r in value['rows']}) != len(value['rows']):
        raise ValueError('duplicate task rows would corrupt denominators')
    timestamp(value['as_of'])
    return value, raw


def prepare(snapshot, protocol, as_of):
    """Retain counts for all rows; fit only mature, attributable actual observations."""
    counts = collections.Counter()
    cohorts = collections.defaultdict(collections.Counter)
    enumerated = snapshot.get('coverage', {}).get('task_enumeration_complete')
    if enumerated is None and protocol['name'] == 'historical-v1':
        # The original exporter asserted complete listing before saving v1.
        enumerated = snapshot.get('tasks_total') is not None
    labels_complete = enumerated is True and 'history_read_failures' in snapshot and not snapshot['history_read_failures']
    links = collections.defaultdict(list)
    for link in snapshot['links']:
        if link.get('observed_proxy') is not None:
            links[link['target']].append(link['observed_proxy'])
        else:
            labels_complete = False
    prepared = []
    plot_rows = []
    for row in snapshot['rows']:
        r = dict(row)
        model = r.get('model')
        identity = json.dumps([model, r.get('effort'), r.get('configuration_digest')], separators=(',', ':'))
        key = json.dumps([r.get('complexity'), identity], separators=(',', ':'))
        cohorts[key]['observed'] += 1
        reason = None
        if r.get('execution_mode') == 'shadow':
            reason = 'shadow_only'
        elif not model or r.get('mixed_models'):
            reason = 'unknown_or_mixed_model'
        elif protocol['name'] != 'historical-v1' and (r.get('effort') is None or not r.get('configuration_digest')):
            reason = 'unknown_effort_or_configuration_digest'
        elif r.get('complexity') not in ('low', 'medium', 'hard', 'xhard'):
            reason = 'unknown_task_tier'
        elif not labels_complete:
            reason = 'incomplete_source_labels'
        elif any(r.get(k) is None for k in ('done', 'landed', 'lines')) or r['lines'] < 0:
            reason = 'missing_size_or_time'
        elif min(r['done'], r['landed']) < timestamp(protocol['cutoff']):
            reason = 'before_cutoff'
        elif r['done'] + 7 * DAY > as_of:
            reason = 'immature_7d'
        if reason:
            counts[reason] += 1
            cohorts[key][reason] += 1
        # Historical replay keeps exact model-only numerical encoding.
        if protocol['name'] != 'historical-v1':
            r['model'] = identity
        if reason and reason != 'before_cutoff':
            r['model'] = None
        prepared.append(r)
        if not reason:
            r['y'] = int(any(r['done'] <= t <= r['done'] + 7 * DAY for t in links[r['id']]))
            plot_rows.append(r)
            cohorts[key]['mature_7d'] += 1
    analysis = dict(snapshot, rows=prepared)
    return analysis, plot_rows, {'source_labels_complete': labels_complete,
                                'exclusions': dict(counts),
                                'cohorts': {k: dict(v) for k, v in cohorts.items()},
                                'mature_ids': sorted(r['id'] for r in plot_rows)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--workspace', help='registered owning workspace selector for read-only current export')
    source.add_argument('--input', type=Path, help='existing v1 JSON or gzip snapshot; no live store access')
    parser.add_argument('--orbit', default='orbit', help='installed or candidate Orbit executable')
    parser.add_argument('--output', type=Path, required=True, help='new explicit scratch directory; must not exist')
    parser.add_argument('--as-of', help='timezone-qualified cutoff, no later than snapshot capture')
    parser.add_argument('--cutoff', help='earliest landing/completion included; timezone-qualified')
    parser.add_argument('--protocol', choices=('observational-v2', 'historical-v1'), default='observational-v2')
    parser.add_argument('--pool', type=Path, help='explicit versioned {schema_version:1,members:[exact identity keys]} pool')
    parser.add_argument('--previous-manifest', type=Path, help='prior manifest fetched through artifact tools')
    parser.add_argument('--export-only', action='store_true', help='export/copy snapshot and manifest without scientific dependencies')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    manifest = {'schema_version': 1, 'status': 'gap', 'protocol': args.protocol,
                'workspace': args.workspace, 'gaps': [], 'files': {},
                'limits': ['Observational/proxy regression links, not causal crew or effort effects.',
                           'Seven-day endpoint is not fourteen-day ladder quality or thirty-day calibration.',
                           'Only completed tasks with size/identity participate; failures/nonlanded work are outside this endpoint.',
                           'Missing links are observed negatives only with complete source coverage, not proven absence of defects.']}
    exit_code = 0
    try:
        now = dt.datetime.now(dt.timezone.utc).isoformat()
        as_of = args.as_of or now
        if timestamp(as_of) > timestamp(now):
            raise ValueError('as-of is in the future')
        cutoff = args.cutoff or '1970-01-01T00:00:00+00:00'
        if args.input:
            snapshot, original = read_snapshot(args.input)
            as_of = args.as_of or snapshot['as_of']
            manifest['source_input_sha256'] = hashlib.sha256(original).hexdigest()
        else:
            from export import export
            snapshot = export(args.workspace, args.orbit, as_of, cutoff)
        if timestamp(as_of) > timestamp(snapshot['as_of']):
            raise ValueError('as-of exceeds source capture; cannot fabricate follow-up')
        snapshot['as_of'] = as_of
        protocol = {'name': args.protocol, 'horizon_days': 7, 'seed_backtest': 20094,
                    'seed_plot': 20095, 'bootstrap_refits': 800, 'permutations': 20000,
                    'pool': [], 'pool_version': None}
        if args.protocol == 'historical-v1':
            protocol['cutoffs'] = HISTORICAL_CUTOFFS
            protocol['cutoff'] = HISTORICAL_CUTOFFS['claimed_filing_fix']
            protocol['pool'] = ['gpt-6-luna', 'claude-haiku-5-5', 'muse-spark-1.3-contributor']
            protocol['pool_version'] = 'original-user-specified-v1'
        else:
            protocol['cutoff'] = cutoff
            protocol['cutoffs'] = {'selected_cutoff': cutoff}
        if args.cutoff and args.protocol == 'historical-v1':
            raise ValueError('historical-v1 cutoffs are frozen; use observational-v2 to change cutoff')
        if args.pool:
            pool = json.loads(args.pool.read_text())
            if pool.get('schema_version') != 1 or not pool.get('version') or not isinstance(pool.get('members'), list):
                raise ValueError('pool requires schema_version 1, version and exact members list')
            protocol['pool'] = sorted(set(pool['members']))
            protocol['pool_version'] = pool['version']
        if timestamp(protocol['cutoff']) > timestamp(as_of):
            raise ValueError('cutoff exceeds as-of')
        raw = json.dumps(snapshot, separators=(',', ':')).encode()
        (output / 'snapshot.json.gz').write_bytes(gzip.compress(raw, mtime=0))
        manifest.update(as_of=as_of, cutoff=protocol['cutoff'], timezone='UTC',
                        input_sha256=hashlib.sha256(raw).hexdigest(), protocol=protocol,
                        source_git_head=snapshot.get('git_head'), source_coverage=snapshot.get('coverage'),
                        source_limitations=snapshot.get('limitations', []))
        analysis, plot_rows, coverage = prepare(snapshot, protocol, timestamp(as_of))
        manifest['coverage'] = coverage
        if args.previous_manifest:
            prior_raw = args.previous_manifest.read_bytes()
            prior = json.loads(prior_raw)
            manifest['prior_review'] = {'sha256': hashlib.sha256(prior_raw).hexdigest(), 'input_sha256': prior.get('input_sha256'),
                                        'as_of': prior.get('as_of'), 'protocol_matches': prior.get('protocol') == protocol}
            manifest['delta'] = {'newly_mature_ids': sorted(set(coverage['mature_ids']) - set(prior.get('coverage', {}).get('mature_ids', []))),
                                 'snapshot_changed': prior.get('input_sha256') != manifest['input_sha256']}
        else:
            manifest['prior_review'] = {'baseline': True, 'reason': 'no previous manifest supplied'}
        if not args.export_only:
            os.environ['MPLCONFIGDIR'] = str(output / 'matplotlib')
            try:
                import numpy  # noqa: F401 -- fail before numerical modules on missing dependency
                import matplotlib  # noqa: F401
            except ImportError as exc:
                raise RuntimeError('missing dependency: ' + str(exc) + '; use the documented environment setup explicitly') from exc
            manifest['environment'] = {'python': sys.version, 'numpy': numpy.__version__, 'matplotlib': matplotlib.__version__}
            from analyze import run as analyze
            from compare import run as compare
            from plot import run as plot
            results = analyze(analysis, output, protocol)
            comparisons = compare(analysis, output, protocol)
            fits = plot(plot_rows, output, protocol)
            # Numeric modules hash their normalized analysis input. Keep the actual
            # source snapshot hash alongside it; never confuse the two cohorts.
            for name, value in [('results.json', results), ('within-tier-results.json', comparisons), ('curve-fits.json', fits)]:
                value['snapshot_sha256'] = manifest['input_sha256']
                write_json(output / name, value)
            manifest['gaps'].extend(fits['gaps'])
            if not plot_rows:
                manifest['gaps'].append('no supported mature actual identity cohort; no revision evidence')
        manifest['status'] = 'complete' if not manifest['gaps'] else 'complete_with_gaps'
    except (ValueError, KeyError, OSError, RuntimeError, sqlite3.Error, subprocess.SubprocessError) as exc:
        manifest['gaps'].append(str(exc))
        exit_code = 2
    report = ['# Crew risk observational report', '', 'Status: ' + manifest['status'], '',
              'This seven-day exploratory endpoint cannot approve tier or effort changes.', '',
              '## Provenance and coverage', '', '```json', json.dumps({k: v for k, v in manifest.items() if k != 'files'}, indent=2), '```', '',
              'Read within-tier-results.json for exact same-tier common-cell comparisons and results.json for temporal held-out metrics.',
              'Read curve-fits.json for supported fits, member counts, and suppressed-curve reasons. Missing quality/cost/effort evidence remains unknown.']
    (output / 'report.md').write_text('\n'.join(report) + '\n')
    for path in sorted(output.iterdir()):
        if path.is_file():
            manifest['files'][path.name] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'bytes': path.stat().st_size}
    manifest['scripts_sha256'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(Path(__file__).parent.glob('*.py'))}
    write_json(output / 'manifest.json', manifest)
    print(json.dumps({'status': manifest['status'], 'manifest': str(output / 'manifest.json'), 'gaps': manifest['gaps']}))
    return exit_code


if __name__ == '__main__':
    sys.exit(main())
