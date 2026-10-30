"""Temporal cloglog backtest from the preserved exploratory protocol. See README."""
import collections, datetime as dt, hashlib, json, math, pathlib
import numpy as np


def run(snapshot, output, protocol):
    ROOT = output
    raw = json.dumps(snapshot, separators=(',', ':')).encode()
    d = snapshot
    DAY = 86400
    def ts(s): return dt.datetime.fromisoformat(s.replace('Z','+00:00')).timestamp()
    ASOF = ts(d['as_of'])
    CUTOFFS = {k: ts(v) for k, v in protocol['cutoffs'].items()}
    incoming = collections.defaultdict(list)
    for e in d['links']: incoming[e['target']].append(e)
    LEVEL = {'low':0,'medium':1,'hard':2,'xhard':3}

    def cohort(cut, horizon=7, review_only=False):
        rows=[]
        for r in d['rows']:
            if not (r['landed'] >= cut and r['done'] >= cut and r['done']+horizon*DAY <= ASOF and r['model']): continue
            rr=dict(r)
            ee=[e for e in incoming[r['id']] if r['done'] <= e['observed_proxy'] <= r['done']+horizon*DAY and (not review_only or set(e['tags']) & {'code-review','delivery-code-review'})]
            rr['y']=int(bool(ee)); rr['count']=len(ee)
            rows.append(rr)
        return sorted(rows,key=lambda r:r['done'])

    def sigmoid_cloglog(eta):
        z=np.exp(np.clip(eta,-20,6))
        p=np.clip(-np.expm1(-z),1e-9,1-1e-9)
        dp=np.maximum(z*np.exp(-z),1e-12)
        return p,dp

    def fit(X,y,pen):
        b=np.zeros(X.shape[1]); rate=np.clip(y.mean(),.001,.999); b[0]=math.log(-math.log(1-rate))
        def objective(bb):
            p,_=sigmoid_cloglog(X@bb)
            return -np.sum(y*np.log(p)+(1-y)*np.log1p(-p))+.5*np.sum(pen*bb*bb)
        last=objective(b)
        for _ in range(80):
            p,dp=sigmoid_cloglog(X@b)
            g=X.T@((p-y)*dp/(p*(1-p)))+pen*b
            w=dp*dp/(p*(1-p))
            H=X.T@(X*w[:,None])+np.diag(pen+1e-7)
            step=np.linalg.solve(H,g)
            scale=1.
            while scale>1e-7:
                trial=b-scale*step; loss=objective(trial)
                if loss<=last+1e-10: break
                scale*=.5
            if scale<=1e-7: break
            b=trial
            if abs(last-loss)<1e-8: break
            last=loss
        return b

    def design(train,test,kind):
        means=np.array([np.log1p(r['lines']) for r in train]); mean=means.mean(); sd=max(means.std(),.1)
        cats=sorted(set(r['complexity'] or 'unknown' for r in train))
        types=sorted(set(r['type'] or 'unknown' for r in train))
        counts=collections.Counter(r['model'] for r in train)
        models=sorted(m for m,n in counts.items() if n>=20)
        def encode(r):
            size=(np.log1p(r['lines'])-mean)/sd
            c=(LEVEL.get(r['complexity'],1)-1)
            x=[1.]; pen=[0.]
            if kind!='constant': x += [size]; pen += [0.]
            if kind in ['complexity','model_intercepts','model_interactions']:
                x += [float((r['complexity'] or 'unknown')==v) for v in cats[1:]]
                pen += [1.]*(len(cats)-1)
                x += [float((r['type'] or 'unknown')==v) for v in types[1:]]
                pen += [1.]*(len(types)-1)
            if kind in ['model_intercepts','model_interactions']:
                m=np.array([float(r['model']==v) for v in models])
                x+=m.tolist(); pen += [10.]*len(models)
                if kind=='model_interactions':
                    x+=(m*size).tolist()+(m*c).tolist(); pen += [20.]*(2*len(models))
            return x,pen
        return np.array([encode(r)[0] for r in train]),np.array([encode(r)[0] for r in test]),np.array(encode(train[0])[1]),models

    def auc(y,p):
        n1=int(y.sum()); n0=len(y)-n1
        if not n1 or not n0: return None
        order=np.argsort(p,kind='stable'); ranks=np.empty(len(y)); start=0
        while start<len(y):
            end=start+1
            while end<len(y) and p[order[end]]==p[order[start]]: end+=1
            ranks[order[start:end]]=(start+1+end)/2; start=end
        return float((ranks[y==1].sum()-n1*(n1+1)/2)/(n1*n0))

    def metrics(y,p):
        ll=-(y*np.log(p)+(1-y)*np.log1p(-p))
        return {'n':len(y),'events':int(y.sum()),'observed_rate':float(y.mean()),'predicted_rate':float(p.mean()),'log_loss':float(ll.mean()),'brier':float(((y-p)**2).mean()),'auc':auc(y,p)}

    KINDS=['constant','size_only','complexity','model_intercepts','model_interactions']
    def evaluate(rows,cut):
        # Fixed seven-day outcomes must mature before each training cutoff.
        fold_start=cut+14*DAY; preds={k:[] for k in KINDS}; folds=[]
        while fold_start<ASOF-7*DAY:
            fold_end=min(fold_start+7*DAY,ASOF-7*DAY)
            train=[r for r in rows if r['done']+7*DAY<=fold_start]
            test=[r for r in rows if fold_start<=r['done']<fold_end]
            if len(train)>=100 and sum(r['y'] for r in train)>=10 and len(test)>=30:
                y=np.array([r['y'] for r in train],dtype=float)
                for kind in KINDS:
                    X,T,pen,models=design(train,test,kind)
                    b=fit(X,y,pen); p,_=sigmoid_cloglog(T@b)
                    preds[kind].extend({'id':r['id'],'fold':len(folds),'y':r['y'],'p':float(pp)} for r,pp in zip(test,p))
                folds.append({'start':dt.datetime.fromtimestamp(fold_start,dt.timezone.utc).isoformat(),'end':dt.datetime.fromtimestamp(fold_end,dt.timezone.utc).isoformat(),'train':len(train),'train_events':int(y.sum()),'test':len(test),'test_events':sum(r['y'] for r in test),'train_models':dict(collections.Counter(r['model'] for r in train))})
            fold_start+=7*DAY
        summary={}
        for kind,pp in preds.items():
            if pp: summary[kind]=metrics(np.array([q['y'] for q in pp]),np.array([q['p'] for q in pp]))
        # Paired fold-cluster bootstrap: few temporal clusters -> weak uncertainty evidence.
        intervals={}
        if preds['size_only'] and len(folds)>=2:
            rng=np.random.default_rng(20094)
            base=preds['size_only']; y=np.array([q['y'] for q in base]); fold=np.array([q['fold'] for q in base]); p=np.array([q['p'] for q in base]); base_loss=-(y*np.log(p)+(1-y)*np.log1p(-p))
            for kind in ['complexity','model_intercepts','model_interactions']:
                p=np.array([q['p'] for q in preds[kind]]); delta=-(y*np.log(p)+(1-y)*np.log1p(-p))-base_loss
                draws=[]
                for _ in range(2000):
                    chosen=rng.integers(0,len(folds),len(folds)); ids=np.concatenate([np.flatnonzero(fold==f) for f in chosen]); draws.append(float(delta[ids].mean()))
                intervals[kind]={'log_loss_delta_vs_size':float(delta.mean()),'fold_cluster_95_percentile_interval':np.quantile(draws,[.025,.975]).tolist(),'clusters':len(folds)}
        return {'folds':folds,'metrics':summary,'deltas':intervals,'predictions':preds}

    report={'input_sha256':hashlib.sha256(raw).hexdigest(),'as_of':d['as_of'],'source_git_head':d['git_head'],'policy_cutoffs_utc':{k:dt.datetime.fromtimestamp(v,dt.timezone.utc).isoformat() for k,v in CUTOFFS.items()},'protocol':{'horizon_days':7,'fold_days':7,'training_outcomes_must_mature_before_test_start':True,'minimum_training_rows':100,'minimum_training_events':10,'minimum_test_rows':30,'model_min_training_support':20,'ridge_model_intercept':10,'ridge_model_interaction':20,'fixed_hyperparameters_no_test_tuning':True,'link_time':'latest relation-related event else finding creation (proxy)','model':'one implementation invocation model before done; mixed excluded; recorded landing model fallback','complexity':'landing snapshot when available, otherwise current assessed task field','primary':'all source types; review-only sensitivity','model_comparison':'cloglog GLM pooled size versus complexity/type versus shrunk model intercepts versus model size/ordinal-complexity interactions'},'coverage':{},'evaluations':{},'limitations':d['limitations']+['Policy commit date is not verified host deployment.','No assignment-time complexity snapshot reconstructed; model identifiers are execution telemetry but cross-host coverage may be incomplete.','Regularized model-specific effects are a screening model, not a fitted class hierarchy or proof of causal capability.','A few week clusters do not support precise confidence claims; intervals are exploratory.','Models without common complexity/size support cannot be compared causally.','Only completed tasks with size and model provenance participate; implementation failure and latent defect counts are not evaluated.']}
    for key,cut in CUTOFFS.items():
        rows=cohort(cut)
        report['coverage'][key]={'mature_7d':len(rows),'events_7d':sum(r['y'] for r in rows),'mature_14d':len(cohort(cut,14)),'by_model':dict(collections.Counter(r['model'] for r in rows)),'model_complexity_counts':{m:dict(collections.Counter(r['complexity'] or 'unknown' for r in rows if r['model']==m)) for m in sorted(set(r['model'] for r in rows))}}
        ev=evaluate(rows,cut); preds=ev.pop('predictions')
        report['evaluations'][key]=ev
        if key=='initial_review_policy':
            (ROOT/'predictions.json').write_text(json.dumps(preds,indent=2))
            review=evaluate(cohort(cut,review_only=True),cut); review.pop('predictions'); report['evaluations']['initial_policy_review_sources_only']=review
    (ROOT/'results.json').write_text(json.dumps(report,indent=2))
    return report
