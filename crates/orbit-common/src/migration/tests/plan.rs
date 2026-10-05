use super::super::*;
use serde_yaml::Mapping;

fn doc(version: u64) -> Value {
    let mut map = Mapping::new();
    map.insert(
        Value::String("schema_version".to_string()),
        Value::Number(version.into()),
    );
    Value::Mapping(map)
}

#[test]
fn rejects_step_that_does_not_bump_version() {
    fn forgetful(value: Value) -> Result<Value, OrbitError> {
        Ok(value)
    }
    let plan = Plan::new("kind", 2).add_step(1, forgetful);
    let err = plan.migrate(doc(1)).expect_err("no bump");
    assert!(
        err.to_string()
            .contains("produced schema_version 1, expected 2"),
        "{err}"
    );
}
