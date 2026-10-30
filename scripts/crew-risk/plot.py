"""Same-tier exploratory size response; numerical fit/bootstraps retain original protocol."""
import collections
import json
import math
import numpy as np

def probability(eta):
    z=np.exp(np.clip(eta,-20,6));p=np.clip(-np.expm1(-z),1e-9,1-1e-9)
    return p,np.maximum(z*np.exp(-z),1e-12)

def fit(x,y):
    if y.sum()==0 or y.sum()==len(y) or x.std()<1e-6:return None
    center=x.mean();scale=max(x.std(),.1); X=np.column_stack([np.ones(len(x)),(x-center)/scale])
    b=np.array([math.log(-math.log(1-y.mean())),0.])
    def loss(bb):
        p,_=probability(X@bb);return -np.sum(y*np.log(p)+(1-y)*np.log1p(-p))
    old=loss(b)
    for _ in range(70):
        p,dp=probability(X@b);g=X.T@((p-y)*dp/(p*(1-p)));w=dp*dp/(p*(1-p))
        try:step=np.linalg.solve(X.T@(X*w[:,None])+np.eye(2)*1e-8,g)
        except np.linalg.LinAlgError:return None
        rate=1.
        while rate>1e-7:
            new=b-rate*step;l=loss(new)
            if l<=old+1e-10:break
            rate*=.5
        if rate<=1e-7:break
        b=new
        if abs(old-l)<1e-8:break
        old=l
    slope=b[1]/scale;intercept=b[0]-slope*center
    if not np.isfinite([intercept,slope]).all() or abs(slope)>40:return None
    return np.array([intercept,slope])


def run(rows, output, protocol):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.ticker import PercentFormatter
    BOOTSTRAPS = 800
    fits = []
    gaps = []
    rng = np.random.default_rng(20095)
    def calculate(tier,m,rs):
        x=np.log1p(np.array([r['lines'] for r in rs],float));y=np.array([r['y'] for r in rs],float)
        b=fit(x,y)
        if b is None: return None
        lower,upper=np.quantile(np.expm1(x),[.05,.95]);grid=np.geomspace(max(lower,1),max(upper,lower+1),120);xx=np.log1p(grid)
        point=probability(b[0]+b[1]*xx)[0];draws=[];coeff=[]
        for _ in range(BOOTSTRAPS):
            ix=rng.integers(0,len(x),len(x));bb=fit(x[ix],y[ix])
            if bb is not None:
                draws.append(probability(bb[0]+bb[1]*xx)[0]);coeff.append(bb.tolist())
        if len(draws) < max(30, BOOTSTRAPS // 2): return None
        bands=np.quantile(np.array(draws),[.025,.975],axis=0)
        # Equal-count bins show observations; only bins with >=10 tasks are drawn.
        bins=[]
        for ix in np.array_split(np.argsort(x),min(5,max(2,len(x)//15))):
            if len(ix)>=10:bins.append({'lines_median':float(np.median(np.expm1(x[ix]))),'n':len(ix),'events':int(y[ix].sum()),'rate':float(y[ix].mean())})
        ci=np.quantile(np.array(coeff)[:,1],[.025,.975])
        out={'tier':tier,'model':m,'label':m,'n':len(rs),'events':int(y.sum()),'composition':dict(collections.Counter(r['model'] for r in rs)),'intercept':float(b[0]),'log_size_slope':float(b[1]),'slope_bootstrap95':ci.tolist(),'bootstrap_requested':BOOTSTRAPS,'bootstrap_finite':len(draws),'lines_5th_95th':[float(lower),float(upper)],'grid_lines':grid.tolist(),'risk':point.tolist(),'band_lower':bands[0].tolist(),'band_upper':bands[1].tolist(),'observed_bins':bins}
        fits.append(out);return out


    tiers = sorted({r['complexity'] for r in rows if r.get('complexity') in {'low','medium','hard','xhard'}})
    if not tiers:
        return {'fits': [], 'gaps': ['no supported mature actual rows; no figures generated']}
    fig, axes = plt.subplots(1, len(tiers), figsize=(7 * len(tiers), 6), squeeze=False)
    for ax, tier in zip(axes[0], tiers):
        group = [r for r in rows if r['complexity'] == tier]
        specs = [(m, [r for r in group if r['model'] == m]) for m in sorted({r['model'] for r in group})]
        if protocol.get('pool'):
            specs.append(('explicit-pool', [r for r in group if r['model'] in protocol['pool']]))
        drawn = 0
        for m, rs in specs:
            k = sum(r['y'] for r in rs)
            if len(rs) < 30 or k < 5 or len(rs) - k < 5:
                gaps.append({'tier': tier, 'identity': m, 'n': len(rs), 'events': k, 'reason': 'requires >=30 rows and >=5 outcomes of each class'})
                continue
            f = calculate(tier, m, rs)
            if f is None:
                gaps.append({'tier': tier, 'identity': m, 'n': len(rs), 'events': k, 'reason': 'singular fit or insufficient finite bootstrap refits'})
                continue
            line, = ax.plot(f['grid_lines'], f['risk'], label=f"{m} (n={len(rs)}, linked={k})")
            ax.fill_between(f['grid_lines'], f['band_lower'], f['band_upper'], color=line.get_color(), alpha=.12)
            for z in f['observed_bins']:
                ax.scatter(z['lines_median'], z['rate'], color=line.get_color(), s=20)
            drawn += 1
        ax.set_title(tier + ' complexity')
        ax.set_xscale('log', base=2)
        ax.set_ylim(0, 1)
        ax.yaxis.set_major_formatter(PercentFormatter(1))
        ax.set_xlabel('Lines added + deleted')
        ax.set_ylabel('Recorded regression link within 7 days')
        if drawn: ax.legend(fontsize=6)
        else: ax.text(.5, .5, 'Insufficient support: curves suppressed', ha='center', transform=ax.transAxes)
    fig.suptitle('Descriptive same-tier size response; seven-day follow-up')
    fig.text(.02, .02, 'Task bootstrap bands; proxy labels and observational routing. Not ladder quality or causal capability.', fontsize=9)
    fig.tight_layout(rect=(0, .06, 1, .93))
    files = []
    # Do not generate a figure when every curve is unsupported.
    if fits:
        for ext in ('png', 'svg', 'pdf'):
            name = 'risk-size-curves.' + ext
            fig.savefig(output / name, dpi=120)
            files.append(name)
    plt.close(fig)
    return {'formula': 'p=1-exp(-exp(a+b*ln(1+lines)))', 'seed': 20095,
            'bootstrap_requested': BOOTSTRAPS, 'files': files, 'fits': fits, 'gaps': gaps}
