//! The profile cache's hit path: a `no-diff-expected` tag edit reclassifies a
//! cached template while no sampled run finishes.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionForecastRecord, CPU_ACCOUNTING_LINUX, DeliveryRunResources, DeliveryRunsFreshness,
    HostResourceMinute, ResourceHistoryKeep, ResourceMinuteLoad, ResourceMinutesTable,
    ResourceUsageStoreBackend, RunResourceMinute,
};

use super::super::{BaseClass, ProfileBounds, ProfileSet, ResourceProfileCache};

/// A store whose sampled runs and freshness are fixed for the whole test.
struct FixedStore {
    runs: Vec<DeliveryRunResources>,
    freshness: DeliveryRunsFreshness,
}

impl ResourceUsageStoreBackend for FixedStore {
    fn record_run_resource_minute(&self, _minute: &RunResourceMinute) -> Result<(), OrbitError> {
        Ok(())
    }

    fn offer_host_resource_minute(&self, _minute: &HostResourceMinute) -> Result<bool, OrbitError> {
        Ok(false)
    }

    fn run_resource_minutes(
        &self,
        _workspace_id: &str,
        _run_id: &str,
    ) -> Result<Vec<RunResourceMinute>, OrbitError> {
        Ok(Vec::new())
    }

    fn delivery_run_resources(
        &self,
        _since: DateTime<Utc>,
    ) -> Result<Vec<DeliveryRunResources>, OrbitError> {
        Ok(self.runs.clone())
    }

    fn delivery_runs_freshness(&self) -> Result<DeliveryRunsFreshness, OrbitError> {
        Ok(self.freshness.clone())
    }

    fn host_resource_minutes(
        &self,
        _since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError> {
        Ok(Vec::new())
    }

    fn record_admission_forecast(
        &self,
        _record: &AdmissionForecastRecord,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn admission_forecasts(
        &self,
        _since: DateTime<Utc>,
    ) -> Result<Vec<AdmissionForecastRecord>, OrbitError> {
        Ok(Vec::new())
    }

    fn resource_minutes_older_than(
        &self,
        _table: ResourceMinutesTable,
        _cutoff: DateTime<Utc>,
        _keep: ResourceHistoryKeep,
    ) -> Result<u64, OrbitError> {
        Ok(0)
    }

    fn prune_resource_minutes_batch(
        &self,
        _table: ResourceMinutesTable,
        _cutoff: DateTime<Utc>,
        _keep: ResourceHistoryKeep,
        _limit: usize,
    ) -> Result<usize, OrbitError> {
        Ok(0)
    }
}

/// One finished, sampled run of a template task: three minutes at half a core.
fn finished_template_run(task_id: &str, admitted_at: DateTime<Utc>) -> DeliveryRunResources {
    DeliveryRunResources {
        workspace_id: "ws".to_string(),
        run_id: format!("run-{task_id}"),
        job_id: Some(format!("job-{task_id}")),
        task_id: task_id.to_string(),
        work_class: "auto-task:nightly".to_string(),
        complexity: None,
        resumed: false,
        delivery_admitted_at: admitted_at,
        finished_at: Some(admitted_at + Duration::minutes(3)),
        crew_model: None,
        orbit_version: None,
        minutes: (0..3)
            .map(|minute| ResourceMinuteLoad {
                minute_started_at: admitted_at + Duration::minutes(minute),
                samples: 1,
                cpu_cores: 0.5,
                cpu_accounting: Some(CPU_ACCOUNTING_LINUX.to_string()),
                memory_bytes: 1 << 30,
            })
            .collect(),
    }
}

fn template_base_class(profiles: &ProfileSet) -> BaseClass {
    profiles
        .profiles
        .iter()
        .find(|profile| profile.key.work_class == "auto-task:nightly")
        .map(|profile| profile.base_class)
        .unwrap()
}

#[test]
fn no_diff_tag_edit_reclassifies_a_cached_template() {
    let admitted_at = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
    let store = FixedStore {
        runs: vec![finished_template_run("T1", admitted_at)],
        freshness: DeliveryRunsFreshness {
            newest_finished_at: Some(admitted_at + Duration::minutes(3)),
            finished_runs: 1,
        },
    };
    let tagged = RefCell::new(BTreeSet::<String>::new());
    let no_diff_tasks = |task_ids: &BTreeSet<String>| -> Result<BTreeSet<String>, OrbitError> {
        Ok(tagged.borrow().intersection(task_ids).cloned().collect())
    };
    let bounds = ProfileBounds {
        window: 10,
        min_deliveries: 1,
        horizon_cap_minutes: 60,
    };
    let now = admitted_at + Duration::hours(1);
    let cache = ResourceProfileCache::default();
    let profiles_now = || {
        cache
            .profiles(&store, &no_diff_tasks, BTreeMap::new(), bounds, now)
            .unwrap()
    };

    let built = profiles_now();
    assert_eq!(template_base_class(&built), BaseClass::Change);

    // Nothing moved: the cache keeps handing out the same profiles.
    let hit = profiles_now();
    assert!(
        Arc::ptr_eq(&built, &hit),
        "an untouched cache must still hit"
    );

    tagged.borrow_mut().insert("T1".to_string());
    assert_eq!(
        template_base_class(&profiles_now()),
        BaseClass::NoDiff,
        "tagging the newest task no-diff-expected must reach the next pass"
    );

    tagged.borrow_mut().remove("T1");
    assert_eq!(
        template_base_class(&profiles_now()),
        BaseClass::Change,
        "removing the tag must reach the next pass"
    );
}
