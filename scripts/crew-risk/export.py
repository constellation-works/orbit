"""Read-only export of a registered owning workspace; no model alias remapping."""
import collections, datetime as dt, gzip, hashlib, json, pathlib, re, sqlite3, subprocess

def run(argv):
    return subprocess.run(argv, capture_output=True, text=True, check=True).stdout

def stamp(x):
    if not x:
        return None
    return dt.datetime.fromisoformat(x.replace('Z', '+00:00')).timestamp()

def export(workspace, orbit, as_of, cutoff):
    def cli(*args):
        return json.loads(run([orbit, *args, '--workspace', workspace, '--json']))
    shown = cli('workspace', 'show')
    if not shown.get('registered') or shown['checkout'].get('role') != 'owner':
        raise ValueError('workspace must be a registered owning checkout; replica/local telemetry is incomplete')
    checkout = pathlib.Path(shown['checkout']['repo_root']).resolve()
    config = cli('config', 'show')
    root = pathlib.Path(config['global_root']).resolve()
    wid = shown['workspace']['id']
    idx = sqlite3.connect((root / 'tasks/index.sqlite').as_uri() + '?mode=ro', uri=True)
    idx.execute('BEGIN')
    bindings = dict(idx.execute('select task_id,canonical_path from task_bundle_bindings where workspace_id=?', (wid,)))
    db = sqlite3.connect((root / 'orbit.db').as_uri() + '?mode=ro', uri=True)
    db.row_factory = sqlite3.Row
    db.execute('BEGIN')
    diffs = {}
    for row in db.execute('select * from landed_diff_stats where workspace_id=? order by landed_at', (wid,)):
        diffs[row['task_id']] = dict(row)
    models = collections.defaultdict(list)
    for row in db.execute("select t.task_id,i.ts,i.model from invocation_tasks t join invocations i on i.id=t.invocation_id where i.activity_id in ('implement_one','agent_implement') and i.model is not null order by i.ts"):
        if row['task_id'] in bindings:
            models[row['task_id']].append([stamp(row['ts']), row['model']])
    payload = {'limit': 10000, 'fields': ['id','status','created_at','updated_at','complexity','type','crew','implemented_by','job_run_id','relations','context_files','tags'], 'model':'codex'}
    listing = cli('tool', 'run', 'orbit.task.list', '--input', json.dumps(payload))
    if listing.get('truncated'):
        payload['limit'] = listing['total']
        listing = cli('tool', 'run', 'orbit.task.list', '--input', json.dumps(payload))
    if listing.get('truncated') or len(listing['tasks']) != listing['total']:
        raise ValueError('task enumeration incomplete; refusing partial regression denominators')
    tasks = listing['tasks']
    for task in tasks:
        if 'relations' not in task:
            detail = cli('tool', 'run', 'orbit.task.show', '--input', json.dumps({'id': task['id'], 'fields': ['id', 'relations'], 'model': 'codex'}))
            if 'relations' not in detail:
                raise ValueError('unread task relations; regression coverage is unknown')
            task['relations'] = detail['relations']
    events = {}
    failures = []
    for t in tasks:
        tid = t['id']
        try:
            events[tid] = [json.loads(x) for x in pathlib.Path(bindings[tid], 'events.jsonl').read_text().splitlines() if x.strip()]
        except Exception as e:
            failures.append([tid, type(e).__name__])
            events[tid] = []

    # Git supplies historical line counts where recorded landed numstats are absent.
    # A subject ID is an attribution proxy; original task -> SHA proof is not reconstructed.
    commits = {}
    current = None
    for line in run(['git','-C',str(checkout),'--no-optional-locks','log',shown['workspace']['base_branch'],'--no-merges','--since='+cutoff,'--numstat','--format=@@%H%x09%cI%x09%s']).splitlines():
        if line.startswith('@@'):
            sha, at, subject = line[2:].split('\t', 2)
            ids = [tid for tid in re.findall(r'\[([^\]]+)\]', subject) if tid in bindings]
            current = {'sha':sha,'at':stamp(at),'lines':0,'files':0} if len(ids)==1 else None
            if current is not None:
                current['id'] = ids[0]
                # git log is newest first: keep the newest attributed commit per task.
                if ids[0] not in commits:
                    commits[ids[0]] = current
        elif current is not None:
            fields = line.split('\t')
            if len(fields)==3 and fields[0].isdigit() and fields[1].isdigit():
                current['lines'] += int(fields[0]) + int(fields[1])
                current['files'] += 1

    links = []
    rows = []
    for t in tasks:
        tid = t['id']
        ev = events[tid]
        done = [stamp(e['at']) for e in ev if e.get('to_status')=='done']
        relation_edits = [stamp(e['at']) for e in ev if 'relation' in (e.get('note') or '').lower() or 'relation' in (e.get('type') or '').lower()]
        link_at = max(relation_edits) if relation_edits else stamp(t['created_at'])
        for rel in t.get('relations', []):
            if rel['type']=='regression_from':
                links.append({'source':tid,'target':rel['target'],'created':stamp(t['created_at']),'observed_proxy':link_at,'relation_edit_found':bool(relation_edits),'tags':t.get('tags', []),'regression':rel.get('regression')})
        if t['status']!='done' or not done:
            continue
        done_at = max(done)
        diff = diffs.get(tid)
        git = commits.get(tid)
        if diff is not None:
            lines = sum(diff[k] for k in ['code_added','code_deleted','tests_added','tests_deleted','other_added','other_deleted'])
            landed_at = stamp(diff['landed_at'])
            basis = 'recorded_landed_diff'
            sha = diff['landed_commit']
        elif git is not None and git['at'] <= done_at + 86400:
            lines = git['lines']
            landed_at = git['at']
            basis = 'git_subject_proxy'
            sha = git['sha']
        else:
            continue
        before = [m for at,m in models.get(tid, []) if at <= done_at]
        unique = sorted(set(before))
        exact = unique[0] if len(unique)==1 else None
        # Recorded landing model is a secondary provenance source; never remap crew aliases.
        source = 'single_implementation_invocation_model' if exact else None
        if exact is None and not unique and diff and diff['model']:
            exact, source = diff['model'], 'recorded_landing_model'
        paths = set()
        for sel in t.get('context_files', []):
            if sel.startswith(('file:', 'dir:', 'symbol:')):
                paths.add(sel.split(':',1)[1].split('#',1)[0])
        rows.append({'id':tid,'done':done_at,'landed':landed_at,'lines':lines,'basis':basis,'sha':sha,'complexity':(diff or {}).get('complexity') or t.get('complexity'),'type':t.get('type'),'crew':t.get('crew'),'effort':None,'configuration_digest':None,'model':exact,'model_basis':source,'mixed_models':unique if len(unique)>1 else [],'declared_paths':len(paths)})

    out = {'schema_version':1,'as_of':as_of,'captured_at':dt.datetime.now(dt.timezone.utc).isoformat(),'workspace_internal_id':wid,'git_head':run(['git','-C',str(checkout),'rev-parse',shown['workspace']['base_branch']]).strip(),'tasks_total':len(tasks),'history_read_failures':failures,'rows':rows,'links':links,'limitations':['Portable continuation of the frozen exploratory protocol; see source-manifest.json.','Historical numstats primarily use commit subjects, not a reconstructed delivery ledger.','Complexity is landing snapshot where recorded, otherwise current task complexity, not guaranteed assignment-time.','Regression observation time is latest relation-related task event, else finding creation; per-edge original payload timing is unavailable.','Absent links are observed negatives, not proven absence of defects.','Mixed implementer models are excluded from primary model comparison; follower invocation coverage may be incomplete.']}
    idx.close()
    db.close()
    out['workspace'] = shown['workspace']['id']
    out['coverage'] = {'task_enumeration_complete': True, 'tasks_total': len(tasks),
                       'history_read_failures': len(failures), 'identity_effort_digest': 'unknown: telemetry does not record these fields',
                       'actual_execution': 'implementation invocation model; landing model fallback is a proxy'}
    return out
