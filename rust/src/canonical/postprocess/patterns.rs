use regex::RegexSet;
use std::sync::LazyLock;

pub(super) static DECORATOR: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
    r#"(?i)app\.(get|post|put|delete|patch|route|websocket|on_event)"#,
    r#"(?i)router\.(get|post|put|delete|patch|route)"#,
    r#"(?i)blueprint\.(route|before_request|after_request)"#,
    r#"(?i)(before|after)_(request|response)"#,
    r#"(?i)click\.(command|group)"#,
    r#"(?i)\w+\.(command|group)\b"#,
    r#"(?i)(field|model)_(serializer|validator)"#,
    r#"(?i)(celery\.)?(task|shared_task|periodic_task)"#,
    r#"(?i)receiver"#,
    r#"(?i)api_view"#,
    r#"(?i)\baction\b"#,
    r#"pytest\.(fixture|mark)"#,
    r#"(?s)^(?:[\w.]+\.)?(?:Test|TestFactory|TestTemplate|ParameterizedTest|RepeatedTest|BeforeEach|AfterEach|BeforeAll|AfterAll|Before|After|BeforeClass|AfterClass|BeforeMethod|AfterMethod|BeforeSuite|AfterSuite|BeforeTest|AfterTest|BeforeGroups|AfterGroups|DataProvider)(?:\s*\(.*)?$"#,
    r#"(?i)(override_settings|modify_settings)"#,
    r#"(?i)(event\.)?listens_for"#,
    r#"(?i)(Get|Post|Put|Delete|Patch|RequestMapping)Mapping"#,
    r#"(?i)(Scheduled|EventListener|Bean|Configuration)"#,
    r#"(?i)KafkaListener"#,
    r#"(?i)(WorkflowMethod|ActivityMethod)"#,
    r#"(?i)(Component|Injectable|Controller|Module|Guard|Pipe)"#,
    r#"(?i)(Subscribe|Mutation|Query|Resolver)"#,
    r#"(app|router)\.(get|post|put|delete|patch|use|all)\b"#,
    r#"(?i)(HiltViewModel|AndroidEntryPoint|Inject)"#,
    r#"(?i)\w+\.(tool|tool_plain|system_prompt|result_validator)\b"#,
    r#"^tool\b"#,
    r#"(?i)\w+\.(middleware|exception_handler|on_exception)\b"#,
    r#"(?i)\w+\.route\b"#,
]).expect("valid built-in patterns")
});
pub(super) static ENTRY: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
    r#"^main$"#,
    r#"^__main__$"#,
    r#"^test_"#,
    r#"^Test[A-Z]"#,
    r#"^on_"#,
    r#"^handle_"#,
    r#"^handler$"#,
    r#"^handle$"#,
    r#"^lambda_handler$"#,
    r#"^upgrade$"#,
    r#"^downgrade$"#,
    r#"^lifespan$"#,
    r#"^get_db$"#,
    r#"^on(Create|Start|Resume|Pause|Stop|Destroy|Bind|Receive)"#,
    r#"^do(Get|Post|Put|Delete)$"#,
    r#"^do_(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)$"#,
    r#"^log_message$"#,
    r#"^(middleware|errorHandler)$"#,
    r#"^ng(OnInit|OnChanges|OnDestroy|DoCheck|AfterContentInit|AfterContentChecked|AfterViewInit|AfterViewChecked)$"#,
    r#"^(transform|writeValue|registerOnChange|registerOnTouched|setDisabledState)$"#,
    r#"^(canActivate|canDeactivate|canActivateChild|canLoad|canMatch|resolve)$"#,
    r#"^(componentDidMount|componentDidUpdate|componentWillUnmount|shouldComponentUpdate|render)$"#,
]).expect("valid built-in patterns")
});
pub(super) static PHP_ENTRY: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([r#"^(boot|register)$"#, r#"^__invoke$"#]).expect("valid built-in patterns")
});
pub(super) static TEST_FILENAME: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r#"^test_.*\.py$"#,
        r#"_test\.py$"#,
        r#"^conftest\.py$"#,
        r#"\.(?:test|spec)\.[cm]?[jt]sx?$"#,
        r#"\.stories\.[jt]sx?$"#,
        r#"_test\.go$"#,
        r#"_(?:test|spec)\.rb$"#,
        r#"[a-z0-9]Tests?\.(?:java|kt|kts|scala|groovy)$"#,
        r#"[a-z0-9]Tests?\.cs$"#,
        r#"[a-z0-9]Test\.php$"#,
        r#"_test\.dart$"#,
        r#"^test[_-].*\.[rR]$"#,
        r#"(?i)\.tests\.ps1$"#,
        r#"[_.]test\.resi?$"#,
        r#"^runtests\.jl$"#,
    ])
    .expect("valid built-in patterns")
});
pub(super) static TEST_DIR: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r#"(?:^|[\\/])tests[\\/]"#,
        r#"(?:^|[\\/])__tests__[\\/]"#,
        r#"(?:^|[\\/])e2e[_-]?tests?[\\/]"#,
        r#"(?:^|[\\/])testthat[\\/]"#,
        r#"(?:^|[\\/])specs?[\\/].*\.(?:rb|rake)$"#,
    ])
    .expect("valid built-in patterns")
});
pub(super) const SECURITY: &[&str] = &[
    "admin",
    "auth",
    "connect",
    "credential",
    "crypt",
    "decrypt",
    "encrypt",
    "execute",
    "hash",
    "http",
    "login",
    "password",
    "permission",
    "privilege",
    "query",
    "request",
    "sanitize",
    "secret",
    "session",
    "sign",
    "socket",
    "sql",
    "token",
    "validate",
    "verify",
];
pub(super) const COMMON_WORDS: &[&str] = &[
    "add", "all", "and", "assert", "at", "be", "build", "by", "create", "delete", "do", "expect",
    "expected", "for", "from", "get", "given", "has", "if", "in", "init", "is", "it", "main",
    "make", "my", "new", "none", "not", "of", "on", "raise", "raises", "remove", "return",
    "returns", "run", "self", "set", "should", "test", "tests", "that", "the", "then", "this",
    "to", "update", "when", "with",
];
