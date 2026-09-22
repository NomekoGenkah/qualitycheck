use std::collections::{BTreeMap, HashMap};
use std::fs;

use qualitycheck::context::{
    gather_candidates, is_test_file, reference_name, ContextPool, Relation, MAX_CONTEXT_CHARS,
    MAX_RELATED_FILE_CHARS,
};
use tempfile::tempdir;

const CONTROLLER: &str = "class Controlador { Servicio servicio; void post(Req r) { servicio.crear(r); } }";
const SERVICE: &str = "class Servicio { void crear(Req r) { validar(r); } }";
const SERVICE_TEST: &str = "class ServicioTest { void rejectsInvalid() { new Servicio().crear(null); } }";

fn pool(files: &[(&str, &str)]) -> ContextPool {
    ContextPool::new(
        files
            .iter()
            .map(|(rel, content)| (rel.to_string(), content.to_string()))
            .collect(),
    )
}

fn related(pool: &ContextPool, target: &str, content: &str) -> Vec<(String, Relation)> {
    pool.related_to(target, content)
        .into_iter()
        .map(|r| (r.path, r.relation))
        .collect()
}

#[test]
fn test_reference_names_and_test_files() {
    assert_eq!(reference_name("src/main/java/Servicio.java").as_deref(), Some("servicio"));
    assert_eq!(reference_name("src/test/java/ServicioTest.java").as_deref(), Some("servicio"));
    assert_eq!(reference_name("app/servicio.spec.ts").as_deref(), Some("servicio"));
    assert_eq!(reference_name("tests/test_user_service.py").as_deref(), Some("userservice"));
    assert_eq!(reference_name("src/UserService.ts").as_deref(), Some("userservice"));
    // Too short or too generic to identify a file in other files' text.
    assert_eq!(reference_name("src/lib.rs"), None);
    assert_eq!(reference_name("src/utils.ts"), None);
    assert_eq!(reference_name("app/index.ts"), None);

    assert!(is_test_file("src/test/java/ServicioTest.java"));
    assert!(is_test_file("app/servicio.spec.ts"));
    assert!(is_test_file("tests/cache_tests.rs"));
    assert!(is_test_file("web/__tests__/form.tsx"));
    assert!(!is_test_file("src/contest.rs"));
    assert!(!is_test_file("src/Servicio.java"));
}

#[test]
fn test_controller_sees_the_service_it_delegates_to() {
    let pool = pool(&[
        ("web/Controlador.java", CONTROLLER),
        ("core/Servicio.java", SERVICE),
        ("core/ServicioTest.java", SERVICE_TEST),
        ("core/Unrelated.java", "class Unrelated {}"),
        ("config/servicio.json", "{ \"servicio\": true }"),
    ]);

    assert_eq!(
        related(&pool, "web/Controlador.java", CONTROLLER),
        vec![("core/Servicio.java".to_string(), Relation::ReferencedByThisFile)]
    );
    assert_eq!(
        related(&pool, "core/Servicio.java", SERVICE),
        vec![
            ("core/ServicioTest.java".to_string(), Relation::TestsThisFile),
            ("web/Controlador.java".to_string(), Relation::ReferencesThisFile),
        ]
    );
    // A test file sees what it tests, never other tests.
    assert_eq!(
        related(&pool, "core/ServicioTest.java", SERVICE_TEST),
        vec![("core/Servicio.java".to_string(), Relation::ReferencedByThisFile)]
    );
}

#[test]
fn test_more_mentioned_files_come_first_within_budget() {
    let big = "x".repeat(MAX_RELATED_FILE_CHARS * 2);
    let target = "Alpha Beta Beta Beta Gamma Gamma";
    let files: BTreeMap<String, String> = [
        ("src/Alpha.java", big.clone()),
        ("src/Beta.java", big.clone()),
        ("src/Gamma.java", big.clone()),
        ("src/Delta.java", big.clone()),
    ]
    .into_iter()
    .map(|(rel, content)| (rel.to_string(), content))
    .collect();
    let related = ContextPool::new(files).related_to("src/Target.java", target);

    let order: Vec<&str> = related.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(order, vec!["src/Beta.java", "src/Gamma.java", "src/Alpha.java"]);
    assert!(related.iter().all(|r| r.truncated));
    let total: usize = related.iter().map(|r| r.content.chars().count()).sum();
    assert!(total <= MAX_CONTEXT_CHARS + related.len() * "\n[... truncated]".len());
}

#[test]
fn test_gather_candidates_finds_referenced_files_across_directories() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    for (rel, content) in [
        ("web/Controlador.java", CONTROLLER),
        ("web/Sibling.java", "class Sibling {}"),
        ("core/Servicio.java", SERVICE),
        ("core/Unrelated.java", "class Unrelated {}"),
        ("test/ControladorTest.java", "class ControladorTest {}"),
    ] {
        fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        fs::write(root.join(rel), content).unwrap();
    }

    let targets = vec![("web/Controlador.java".to_string(), CONTROLLER.to_string())];
    let candidates = gather_candidates(root, &targets, &HashMap::new(), 200);
    let paths: Vec<&str> = candidates.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        vec![
            "core/Servicio.java",
            "test/ControladorTest.java",
            "web/Controlador.java",
            "web/Sibling.java",
        ]
    );

    // The base side of a delta sees base versions, and not files that didn't exist yet.
    let overrides = HashMap::from([
        ("core/Servicio.java".to_string(), Some("class Servicio { /* v1 */ }".to_string())),
        ("web/Sibling.java".to_string(), None),
    ]);
    let base = gather_candidates(root, &targets, &overrides, 200);
    assert_eq!(base["core/Servicio.java"], "class Servicio { /* v1 */ }");
    assert!(!base.contains_key("web/Sibling.java"));
}
