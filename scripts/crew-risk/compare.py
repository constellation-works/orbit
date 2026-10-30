"""Descriptive within-tier comparison on the frozen attribution-aware snapshot.

This is an observational comparison of seven-day linked outcomes, not a causal
model-capability ranking or a forecasting benchmark. No production state changes.
"""
import collections
import datetime as dt
import hashlib
import itertools
import json
import math
import pathlib
import statistics
import numpy as np


def run(snapshot, output, protocol):
    ROOT = output
    raw = json.dumps(snapshot, separators=(',', ':')).encode()
    d = snapshot
    ASOF = dt.datetime.fromisoformat(d['as_of']).timestamp()
    DAY = 86400
    CUTOFF = dt.datetime.fromisoformat(protocol['cutoff']).timestamp()
    MIN_TASKS = 30
    MIN_EVENTS = 5
    PERMUTATIONS = 20000
    SMALL_MODELS = set(protocol.get('pool', []))
    edges = collections.defaultdict(list)
    for e in d['links']:
        edges[e['target']].append(e)

    def make_rows(review_only=False):
        out = []
        for r in d['rows']:
            if not (r['model'] and min(r['done'], r['landed']) >= CUTOFF and r['done'] + 7*DAY <= ASOF):
                continue
            r = dict(r)
            links = [e for e in edges[r['id']] if r['done'] <= e['observed_proxy'] <= r['done']+7*DAY and (not review_only or set(e['tags']) & {'code-review','delivery-code-review'})]
            r['y'] = int(bool(links))
            r['week'] = dt.datetime.fromtimestamp(r['done'],dt.timezone.utc).strftime('%G-W%V')
            # Exact calendar-week/type/size-bin overlap avoids extrapolating across cells.
            r['size_bin'] = int(math.floor(math.log2(1+r['lines'])))
            out.append(r)
        assert len(out) == len({r['id'] for r in out})
        return out

    def wilson(n, k):
        z = 1.959963984540054
        p = k/n
        den = 1+z*z/n
        center = (p+z*z/(2*n))/den
        half = z*math.sqrt(p*(1-p)/n+z*z/(4*n*n))/den
        return [max(0.,center-half),min(1.,center+half)]

    def summary(rs):
        n=len(rs); k=sum(r['y'] for r in rs)
        return {'n':n,'events':k,'rate':k/n,'wilson95':wilson(n,k),'median_lines':statistics.median(r['lines'] for r in rs),'weeks':dict(collections.Counter(r['week'] for r in rs)),'types':dict(collections.Counter(r['type'] for r in rs)),'qualifies':n>=MIN_TASKS,'sparse_events':k<MIN_EVENTS}

    def compare(a,b,seed):
        def cell(r): return (r['week'],r['type'] or 'unknown',r['size_bin'])
        aa=collections.defaultdict(list); bb=collections.defaultdict(list)
        for r in a: aa[cell(r)].append(r['y'])
        for r in b: bb[cell(r)].append(r['y'])
        keys=sorted(aa.keys() & bb.keys())
        cells=[]
        for key in keys:
            na=len(aa[key]);nb=len(bb[key]);ka=sum(aa[key]);kb=sum(bb[key])
            cells.append({'week':key[0],'type':key[1],'size_bin':key[2],'a_n':na,'b_n':nb,'a_events':ka,'b_events':kb,'weight':min(na,nb)})
        if not cells: return {'comparable':False,'reason':'no common calendar-week/type/size cell'}
        weights=np.array([c['weight'] for c in cells],dtype=float);weights/=weights.sum()
        na=np.array([c['a_n'] for c in cells]);nb=np.array([c['b_n'] for c in cells]);ka=np.array([c['a_events'] for c in cells]);kb=np.array([c['b_events'] for c in cells])
        ra=float(weights@(ka/na));rb=float(weights@(kb/nb));diff=ra-rb
        rng=np.random.default_rng(seed)
        # Conditional randomization is a descriptive association test: assignment was
        # not randomized, so these p-values cannot establish causal capability.
        shuffled=rng.hypergeometric(ka+kb,na+nb-ka-kb,na,size=(PERMUTATIONS,len(cells)))
        null=((shuffled/na-(ka+kb-shuffled)/nb)*weights).sum(axis=1)
        p=(1+int((np.abs(null)>=abs(diff)-1e-12).sum()))/(PERMUTATIONS+1)
        effective=int(sum(c['weight'] for c in cells));events=int(ka.sum()+kb.sum())
        adequate=effective>=20 and int(na.sum())>=30 and int(nb.sum())>=30 and events>=10
        return {'comparable':True,'adequate_overlap_screen':adequate,'a_overlap_n':int(na.sum()),'b_overlap_n':int(nb.sum()),'a_overlap_events':int(ka.sum()),'b_overlap_events':int(kb.sum()),'balanced_cell_weight_per_side':effective,'common_cells':len(cells),'a_standardized_rate':ra,'b_standardized_rate':rb,'standardized_difference':diff,'conditional_permutation_p':p,'cells':cells}

    rows=make_rows()
    groups=collections.defaultdict(lambda:collections.defaultdict(list))
    for r in rows:groups[r['complexity'] or 'unknown'][r['model']].append(r)
    result={'as_of':d['as_of'],'input_sha256':hashlib.sha256(raw).hexdigest(),'cohort':'completed tasks after explicit cutoff with seven-day follow-up and attributable execution identity','thresholds':{'raw_min_tasks':MIN_TASKS,'raw_min_events':MIN_EVENTS,'overlap_min_tasks_each':30,'balanced_cell_weight_min':20,'common_cell_events_min':10},'method':'Raw within-tier rates and Wilson intervals; pair-specific direct standardization over exact common UTC calendar week x task type x floor(log2(1+lines)) cells, weighted by min(nA,nB). Conditional within-cell label permutation; Holm adjusted over all qualifying tier-pairs. These are task-level association tests, not causal estimates; related-task dependence is unmodeled.','tiers':{},'pairs':[],'limitations':d['limitations']+['Complexity is not guaranteed an assignment-time snapshot.','Source-policy date is not a verified deployment cutoff.','Common-cell standardization conditions on model-produced final size and compares different target populations per pair.','Seven-day recorded outcomes miss later or unreported regressions; failed/nonlanded work is excluded.','Task-level confidence intervals and permutation tests do not model related-task clustering or unobserved routing/reviewer differences.','Raw >=30 tasks and >=5 events is an exploratory reporting threshold, not a guarantee of sufficient precision.']}
    review_groups=collections.defaultdict(list)
    for r in make_rows(True):review_groups[(r['complexity'],r['model'])].append(r)
    for tier in ['low','medium','hard','xhard']:
        group=groups[tier]
        result['tiers'][tier]={m:summary(rs) for m,rs in sorted(group.items(),key=lambda kv:-len(kv[1]))}
        models=sorted(m for m,rs in group.items() if len(rs)>=MIN_TASKS)
        for a,b in itertools.combinations(models,2):
            comp=compare(group[a],group[b],20094+len(result['pairs']))
            comp.update({'tier':tier,'a':a,'b':b})
            rev=compare(review_groups[(tier,a)],review_groups[(tier,b)],30094+len(result['pairs']))
            comp['review_sources_only']={k:v for k,v in rev.items() if k!='cells'}
            result['pairs'].append(comp)

    pairs=[p for p in result['pairs'] if p.get('comparable')]
    ranked=sorted(pairs,key=lambda p:p['conditional_permutation_p'])
    last=0.
    for rank,p in enumerate(ranked):
        last=max(last,min(1.,p['conditional_permutation_p']*(len(ranked)-rank)))
        p['holm_adjusted_p']=last

    # User-authorized class pooling never mixes task-complexity tiers or model versions.
    # Keep members visible; do not compare a pool against its own component models.
    result['small_model_pool']={}
    for tier in ['low','medium','hard','xhard']:
        pooled=[r for r in rows if r['complexity']==tier and r['model'] in SMALL_MODELS]
        if not pooled:
            result['small_model_pool'][tier]={'n':0,'members':{}}
            continue
        s=summary(pooled)
        s['members']={m:summary([r for r in pooled if r['model']==m]) for m in sorted(SMALL_MODELS) if any(r['model']==m for r in pooled)}
        s['comparisons']=[]
        if s['qualifies']:
            for m,rs in groups[tier].items():
                if m in SMALL_MODELS or len(rs)<MIN_TASKS:
                    continue
                comp=compare(pooled,rs,40094+len(s['comparisons']))
                comp['other_model']=m
                s['comparisons'].append(comp)
        result['small_model_pool'][tier]=s
    # Pool comparisons are additional, post-request descriptive screens. No significance
    # claim is made for them; the individual-model Holm family above remains unchanged.
    result['thresholds']['raw_min_events']=None
    result['thresholds']['sparse_outcome_warning_below_events']=MIN_EVENTS
    result['limitations'].append('An explicit pool is a composition-weighted average, not evidence of equal member capability; per-member counts remain visible. No pooled p-value is interpreted as confirmatory.')
    result['pool_version'] = protocol.get('pool_version')

    (ROOT/'within-tier-results.json').write_text(json.dumps(result,indent=2))
    return result
