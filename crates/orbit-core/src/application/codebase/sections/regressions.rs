use super::*;

use super::rollup::*;

#[derive(Default)]
pub(in crate::application::codebase) struct RegressionRow<'a> {
    pub(in crate::application::codebase) tally: Tally,
    /// The regressed tasks: the numerator.
    pub(in crate::application::codebase) regressed: Vec<&'a TaskFacts>,
    /// Every task the row counts: the denominator.
    pub(in crate::application::codebase) touching: Vec<&'a TaskFacts>,
    by_crew: BTreeMap<String, Tally>,
    /// Each regressed task's recorded sites under the path, by task ID.
    pub(in crate::application::codebase) sites: BTreeMap<&'a str, BTreeSet<&'a str>>,
    /// Regressed tasks that declared the path but whose kept links record no
    /// site, so they count against no path.
    pub(in crate::application::codebase) unsited: Vec<&'a TaskFacts>,
}

impl<'a> RegressionRow<'a> {
    fn count_touching(&mut self, task: &'a TaskFacts, crew: &str) {
        self.tally.denominator += 1;
        self.touching.push(task);
        self.by_crew
            .entry(crew.to_string())
            .or_default()
            .denominator += 1;
    }

    fn count_regressed(&mut self, task: &'a TaskFacts, crew: &str) {
        self.tally.numerator += 1;
        self.regressed.push(task);
        self.by_crew.entry(crew.to_string()).or_default().numerator += 1;
    }
}

pub(in crate::application::codebase) fn crew_of(task: &TaskFacts) -> String {
    task.crew
        .clone()
        .unwrap_or_else(|| "unassigned".to_string())
}

/// The Regressions ranking's rows, the done tasks of the window, and the
/// regressed tasks and links the filter keeps that record no site.
pub(in crate::application::codebase) struct RegressionRows<'a> {
    pub(in crate::application::codebase) rows: BTreeMap<String, RegressionRow<'a>>,
    population: usize,
    unsited_tasks: usize,
    unsited_links: usize,
}

/// The sites a link records, each with the path it names; a site that names
/// no path is no site.
fn link_sites(link: &RegressionLink) -> impl Iterator<Item = (&str, Anchor)> {
    link.sites.iter().filter_map(|site| {
        let anchor = Anchor::parse(site).or_else(|| match Selector::from_str(site).ok()? {
            Selector::Dir { path } if path == "." => Some(Anchor { path, dir: true }),
            _ => None,
        })?;
        Some((site.as_str(), anchor))
    })
}

/// Site-attributed regressions: distinct done tasks counted against a path
/// because a `regression_from` link the filter keeps names a site under it
/// ÷ done tasks whose footprint touched the path, for tasks that became done
/// in the window.
///
/// A `symbol:` or `file:` site counts under its file's key at the rollup; a
/// non-root `dir:` site under its own key and every footprint or site key below
/// it. The root `dir:.` site covers all those keys without creating a root-only
/// key. A regressed task counts once per key however many sites name it, and
/// joins the denominator of a key it did not declare, so the rate stays at
/// most 1. Links without a site are attributed to no path: they are counted,
/// with their regressed tasks, as unsited.
pub(in crate::application::codebase) fn regression_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> RegressionRows<'a> {
    let mut rows: BTreeMap<String, RegressionRow> = BTreeMap::new();
    let mut population = 0;
    let mut unsited_tasks = 0;
    let mut unsited_links = 0;
    let mut sited = Vec::new();
    for task in &facts.tasks {
        if !task.done_at.is_some_and(|at| params.within(at)) {
            continue;
        }
        population += 1;
        let links = params.filter.links(task);
        let sites: BTreeMap<&str, Anchor> =
            links.iter().flat_map(|link| link_sites(link)).collect();
        unsited_links += links
            .iter()
            .filter(|link| link_sites(link).next().is_none())
            .count();
        let unsited = !links.is_empty() && sites.is_empty();
        unsited_tasks += usize::from(unsited);
        let crew = crew_of(task);
        let declared = params.rollup.keys(&task.footprint);
        for key in &declared {
            let row = rows.entry(key.clone()).or_default();
            row.count_touching(task, &crew);
            if unsited {
                row.unsited.push(task);
            }
        }
        if !sites.is_empty() {
            // Seed every explicit site key before expanding directory sites,
            // including keys outside all done tasks' declared footprints.
            for anchor in sites.values() {
                if !(anchor.dir && anchor.path == ".") {
                    rows.entry(params.rollup.key(anchor)).or_default();
                }
            }
            sited.push((task, crew, declared, sites));
        }
    }
    let candidate_keys: Vec<String> = rows.keys().cloned().collect();
    for (task, crew, declared, sites) in sited {
        let mut keys: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for (site, anchor) in &sites {
            let is_root_dir = anchor.dir && anchor.path == ".";
            if !is_root_dir {
                keys.entry(params.rollup.key(anchor))
                    .or_default()
                    .insert(site);
            }
            if anchor.dir {
                if is_root_dir {
                    for key in &candidate_keys {
                        keys.entry(key.clone()).or_default().insert(site);
                    }
                } else {
                    let under = format!("{}/", anchor.path);
                    for key in candidate_keys.iter().filter(|key| key.starts_with(&under)) {
                        keys.entry(key.clone()).or_default().insert(site);
                    }
                }
            }
        }
        for (key, key_sites) in keys {
            let row = rows.entry(key.clone()).or_default();
            if !declared.contains(&key) {
                row.count_touching(task, &crew);
            }
            row.count_regressed(task, &crew);
            row.sites.insert(task.id.as_str(), key_sites);
        }
    }
    RegressionRows {
        rows,
        population,
        unsited_tasks,
        unsited_links,
    }
}

/// Footprint-attributed regressions, which regression-risk marks read:
/// distinct done tasks with an incoming `regression_from` the filter keeps ÷
/// done tasks whose footprint touched the path, for tasks that became done in
/// the window. A regressed task counts against every path it declared.
pub(in crate::application::codebase) fn footprint_regression_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> BTreeMap<String, RegressionRow<'a>> {
    let mut rows: BTreeMap<String, RegressionRow> = BTreeMap::new();
    for task in &facts.tasks {
        if !task.done_at.is_some_and(|at| params.within(at)) {
            continue;
        }
        let regressed = params.filter.regressed(task);
        let crew = crew_of(task);
        for key in params.rollup.keys(&task.footprint) {
            let row = rows.entry(key).or_default();
            row.count_touching(task, &crew);
            if regressed {
                row.count_regressed(task, &crew);
            }
        }
    }
    rows
}

pub(in crate::application::codebase) fn regressions(
    facts: &CodebaseFacts,
    params: &Params,
) -> Value {
    let RegressionRows {
        rows,
        population,
        unsited_tasks,
        unsited_links,
    } = regression_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, mut row)| {
            row.regressed
                .sort_by_key(|regressed| std::cmp::Reverse(regressed.done_at));
            let regressed: Vec<Value> = row
                .regressed
                .iter()
                .take(DRILLDOWN_LIMIT)
                .map(|task| {
                    merge(
                        json!({ "task_id": task.id, "title": task.title }),
                        regressed_fields(task, &row, &params.filter),
                    )
                })
                .collect();
            let mut sites = site_entries(&row, &params.filter);
            sites.truncate(DRILLDOWN_LIMIT);
            let mut by_crew: Vec<(String, Tally)> = row.by_crew.into_iter().collect();
            by_crew.sort_by(|(left_crew, left), (right_crew, right)| {
                right
                    .numerator
                    .cmp(&left.numerator)
                    .then_with(|| right.denominator.cmp(&left.denominator))
                    .then_with(|| left_crew.cmp(right_crew))
            });
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({
                    "path": path,
                    "regressed_tasks": regressed,
                    "sites": sites,
                    "by_crew": by_crew
                        .into_iter()
                        .map(|(crew, tally)| merge(
                            rate_fields(tally.numerator, tally.denominator),
                            json!({ "crew": crew }),
                        ))
                        .collect::<Vec<_>>(),
                }),
            )
        })
        .collect();
    let mut section = merge(
        json!({
            "rows": rows,
            "below_support": below_support,
            "population": population,
            "attribution": "sites",
            "unsited": { "tasks": unsited_tasks, "links": unsited_links },
        }),
        super::super::regression::summary(facts, params),
    );
    section["risk_marks"] = super::super::risk::comparison(facts, params);
    section
}

/// What a regressed task's entry adds: its crew and complexity, the links
/// the filter keeps, as IDs and with their four dimensions, and the sites
/// that attribute it to `row`'s path.
pub(in crate::application::codebase) fn regressed_fields(
    task: &TaskFacts,
    row: &RegressionRow,
    filter: &RegressionFilter,
) -> Value {
    let links = filter.links(task);
    let sites: Vec<&str> = row
        .sites
        .get(task.id.as_str())
        .into_iter()
        .flatten()
        .copied()
        .collect();
    json!({
        "crew": task.crew,
        "complexity": task.complexity,
        "regression_tasks": links.iter().map(|link| &link.task_id).collect::<Vec<_>>(),
        "regressions": links.iter().map(|link| link.json()).collect::<Vec<_>>(),
        "sites": sites,
    })
}

/// The distinct sites that attribute `row`'s regressed tasks to its path,
/// most regressed tasks first, then most links, then by selector. Each names
/// its scope (`symbol`, `file` or `dir`) with the symbol's name and kind, the
/// regressed tasks it attributes, and the kept links naming it with their
/// regression tasks.
pub(in crate::application::codebase) fn site_entries(
    row: &RegressionRow,
    filter: &RegressionFilter,
) -> Vec<Value> {
    #[derive(Default)]
    struct Site<'a> {
        regressed: Vec<&'a str>,
        links: usize,
        regression_tasks: BTreeSet<&'a str>,
    }
    let mut by_site: BTreeMap<&str, Site> = BTreeMap::new();
    for task in &row.regressed {
        let Some(sites) = row.sites.get(task.id.as_str()) else {
            continue;
        };
        let links = filter.links(task);
        for site in sites {
            let entry = by_site.entry(site).or_default();
            entry.regressed.push(task.id.as_str());
            for link in links
                .iter()
                .filter(|link| link.sites.iter().any(|named| named == site))
            {
                entry.links += 1;
                entry.regression_tasks.insert(link.task_id.as_str());
            }
        }
    }
    let mut sites: Vec<(&str, Site)> = by_site.into_iter().collect();
    sites.sort_by(|(left_site, left), (right_site, right)| {
        right
            .regressed
            .len()
            .cmp(&left.regressed.len())
            .then_with(|| right.links.cmp(&left.links))
            .then_with(|| left_site.cmp(right_site))
    });
    sites
        .into_iter()
        .map(|(site, entry)| {
            let (scope, path, symbol, kind) = match Selector::from_str(site) {
                Ok(Selector::Symbol { path, symbol, kind }) => {
                    ("symbol", path, Some(symbol), Some(kind))
                }
                Ok(Selector::Dir { path }) => ("dir", path, None, None),
                Ok(Selector::File { path }) => ("file", path, None, None),
                _ => ("file", site.to_string(), None, None),
            };
            json!({
                "selector": site,
                "scope": scope,
                "path": path,
                "symbol": symbol,
                "kind": kind,
                "tasks": entry.regressed.len(),
                "links": entry.links,
                "regressed_tasks": entry.regressed,
                "regression_tasks": entry.regression_tasks,
            })
        })
        .collect()
}
