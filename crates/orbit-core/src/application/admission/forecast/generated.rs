//! Instantiate forecast-only nodes from already frozen donor conflict rows.

use super::*;

impl ForecastAdmission {
    /// Materialize initial tasks followed by sampled donor instances. This
    /// never expands a selector or reads live state. Duplicate editing donors
    /// conflict with each other, while no-diff donors retain empty locks.
    pub(crate) fn with_forecast_generated_nodes(
        &self,
        templates: &[usize],
        initial: usize,
    ) -> Self {
        let len = templates.len();
        let tasks: Vec<Task> = templates
            .iter()
            .enumerate()
            .map(|(index, template)| {
                let mut task = self.tasks[*template].clone();
                if index >= initial {
                    task.id = format!("forecast-node-{index}");
                    task.status = TaskStatus::Backlog;
                    task.relations.clear();
                }
                task
            })
            .collect();
        let rows = |source: &[ForecastTaskSet], held: bool| {
            templates
                .iter()
                .enumerate()
                .map(|(own, template)| {
                    let mut row = ForecastTaskSet::with_len(len);
                    for (other, donor) in templates.iter().enumerate() {
                        if own != other
                            && (source[*template].contains(*donor)
                                || (template == donor
                                    && self.edits[*donor]
                                    && (!held
                                        || !orbit_types::task::is_no_diff_expected(
                                            &self.tasks[*donor].tags,
                                        ))))
                        {
                            row.insert(other);
                        }
                    }
                    row
                })
                .collect()
        };
        let copy = |values: &[bool]| templates.iter().map(|index| values[*index]).collect();
        let mut rank = vec![0; self.len()];
        for (position, index) in self.order.iter().enumerate() {
            rank[*index] = position
        }
        let mut order: Vec<usize> = (0..len).collect();
        order.sort_by_key(|index| (rank[templates[*index]], *index));
        let no_footprint: Vec<bool> = templates
            .iter()
            .enumerate()
            .map(|(index, donor)| index < initial && self.no_footprint[*donor])
            .collect();
        let mut no_footprint_set = ForecastTaskSet::with_len(len);
        for (index, none) in no_footprint.iter().enumerate() {
            if *none {
                no_footprint_set.insert(index)
            }
        }
        Self {
            lookup: tasks
                .iter()
                .map(|task| (task.id.clone(), task.clone()))
                .collect(),
            held: rows(&self.held, true),
            surface: rows(&self.surface, false),
            edits: copy(&self.edits),
            no_footprint,
            footprint_deadline: templates
                .iter()
                .enumerate()
                .map(|(index, donor)| {
                    if index < initial {
                        self.footprint_deadline[*donor]
                    } else {
                        None
                    }
                })
                .collect(),
            exclusive_waits: self.exclusive_waits.clone(),
            no_footprint_set,
            root: templates
                .iter()
                .enumerate()
                .map(|(index, donor)| {
                    if index < initial {
                        self.root[*donor]
                    } else {
                        self.roots + index
                    }
                })
                .collect(),
            roots: self.roots + len,
            reserves: copy(&self.reserves),
            expiring: self.expiring.clone(),
            forecast_at: self.forecast_at,
            surface_hash: templates
                .iter()
                .map(|index| self.surface_hash[*index].clone())
                .collect(),
            review_lock_overlaps: copy(&self.review_lock_overlaps),
            resource_fit: self
                .resource_fit
                .as_ref()
                .map(|fit| fit.with_forecast_generated_nodes(templates)),
            tasks,
            order,
        }
    }
}
