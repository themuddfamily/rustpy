//! Built-in vocabulary that is never reported: programming jargon, Python
//! keywords and builtins, dunder names, and tool directives in comments.

use std::sync::OnceLock;

/// Programming jargon and abbreviations that English dictionaries lack (all
/// lowercase ASCII letters). Words that the bundled dictionary already has are
/// listed too where they matter, so that [`crate::check_text`] skips them
/// whatever dictionary it is given.
#[rustfmt::skip]
const PROGRAMMING_JARGON: &[&str] = &[
    // Python language and standard library
    "abc", "abcs", "abspath", "aenter", "aexit", "aiter", "anext", "argc", "argparse", "args",
    "argv", "asgi", "async", "asyncio", "attr", "attrs", "await", "awaitable", "awaitables",
    "bdist", "bool", "bools", "builtin", "builtins", "bytearray", "bytecode", "bytecodes",
    "bytestring", "bytestrings", "classmethod", "classmethods", "cls", "codec", "codecs",
    "conftest", "contextlib", "contextmanager", "contextvar", "contextvars", "coro", "coroutine",
    "coroutines", "cpython", "ctypes", "cython", "dataclass", "dataclasses", "datetime",
    "datetimes", "deepcopy", "defaultdict", "del", "delattr", "dict", "dicts", "distutils",
    "docstring", "docstrings", "doctest", "doctests", "dunder", "dunders", "elif", "enum", "enums",
    "excepthook", "exc", "frozenset", "functools", "getattr", "getattribute", "getitem", "hasattr",
    "hashable", "hashlib", "importlib", "init", "inits", "ipynb", "ipython", "isinstance",
    "issubclass", "iter", "iterable", "iterables", "iters", "itertools", "jupyter", "jython",
    "kwarg", "kwargs", "kwonly", "len", "metaclass", "metaclasses", "micropython", "monkeypatch",
    "mro", "namedtuple", "nonlocal", "noqa", "numpy", "pathlib", "pep", "peps", "pickleable",
    "posonly", "pyc", "pyd", "pyi", "pyo", "pyproject", "pypy", "pythonic", "pythonpath", "pyw",
    "pyx", "pyz", "repr", "reprs", "setattr", "setitem", "sdist", "stdlib", "staticmethod",
    "staticmethods", "str", "strs", "subclass", "subclassed", "subclasses", "subclassing",
    "subprocess", "subprocesses", "superclass", "superclasses", "tomllib", "traceback",
    "tracebacks", "tuple", "tuples", "typeshed", "unittest", "urllib", "varargs", "varkw", "venv",
    "venvs", "virtualenv", "virtualenvs", "walrus", "wsgi", "eventloop", "falsey", "falsy",
    "monkeypatching", "namedtuples", "nogil", "picklable", "refcount", "refcounting", "refcounts",
    "subclassable", "subinterpreter", "subinterpreters", "subtests", "typevar", "typevars",
    "unhashable", "unittests", "unpicklable", "weakrefs",
    // Python ecosystem and tools
    "aiohttp", "alembic", "autopep", "black", "boto", "botocore", "celery", "conda", "dask",
    "django", "fastapi", "flake", "flask", "gunicorn", "httpx", "isort", "jinja", "keras", "lxml",
    "matplotlib", "mkdocs", "msgpack", "mypy", "networkx", "nox", "numba", "opencv", "orjson",
    "pandas", "pipenv", "pipx", "polars", "psycopg", "pyarrow", "pycharm", "pydantic", "pyenv",
    "pyflakes", "pygame", "pyinstaller", "pylint", "pymysql", "pypi", "pyplot", "pyqt", "pyre",
    "pyright", "pyspark", "pytest", "pytorch", "pytype", "pyyaml", "readthedocs", "ruff", "scipy",
    "scrapy", "seaborn", "setuptools", "sklearn", "sphinx", "sqlalchemy", "starlette", "sympy",
    "tensorflow", "tkinter", "tomli", "torch", "tox", "ttk", "ujson", "uvicorn", "xarray", "yapf",
    "autoflake", "autoreload", "huggingface", "pylance", "pyupgrade",
    // Editors, platforms and languages
    "bash", "cargo", "clang", "cmake", "csharp", "dockerfile", "emacs", "gcc", "github", "gitlab",
    "golang", "intellij", "java", "javascript", "jetbrains", "jdk", "jre", "jvm", "kotlin", "linux",
    "llvm", "macos", "msvc", "neovim", "nginx", "nodejs", "npm", "php", "pnpm", "posix",
    "powershell", "rustc", "typescript", "unix", "vim", "vscode", "webpack", "zsh", "ansible",
    "bitbucket", "glibc", "grafana", "htmx", "jira", "jsx", "kubectl", "mongodb", "musl",
    "rabbitmq", "scss", "sysroot", "terraform", "tsx", "vue", "xcode", "dbus", "debian", "distro",
    "distros", "dpkg", "objdump", "systemd", "ubuntu",
    // Data formats, protocols and file types
    "ascii", "base", "bmp", "bz", "cdn", "cors", "crlf", "csrf", "css", "csv", "csvs", "dns", "eof",
    "eol", "eols", "ftp", "gif", "gifs", "graphql", "grpc", "gz", "gzip", "hmac", "htm", "html",
    "http", "https", "ico", "imap", "ini", "ipv", "jpeg", "jpg", "json", "jsonl", "jwt", "lf",
    "lzma", "md", "mime", "mimetype", "mimetypes", "oauth", "ogg", "pdf", "pdfs", "png", "pngs",
    "protobuf", "rgb", "rgba", "rpc", "rst", "sftp", "sha", "smtp", "sql", "sqlite", "ssh", "ssl",
    "svg", "svgs", "tar", "tcp", "tgz", "tiff", "tls", "toml", "tsv", "txt", "udp", "uri", "uris",
    "url", "urls", "utc", "utf", "uuid", "uuids", "wasm", "wav", "webp", "xhtml", "xml", "xpath",
    "xss", "xz", "yaml", "yml", "zlib", "zstd", "ldap", "mfa", "openapi", "otp", "printf", "saml",
    "sso", "totp", "dhcp", "pkcs", "urlencoded",
    // General programming vocabulary
    "codename", "codenames", "datasource", "datasources", "errback", "errbacks", "exprs",
    "highlighter", "highlighters", "iface", "ifaces", "keyring", "keyrings", "nameserver",
    "nameservers", "passphrase", "passphrases", "prereleases", "renderable", "renderables",
    "userdata",
    "autoscaling", "bitset", "bitsets", "callee", "callees", "camelcase", "changeset", "changesets",
    "classpath", "datapoint", "datapoints", "deserializable", "destructure", "destructuring",
    "dockerize", "dockerized", "encodable", "entrypoints", "fieldname", "fieldnames", "finalizers",
    "hashcode", "hotfix", "hotfixes", "jsonify", "lifecycle", "lifecycles", "microservice",
    "microservices", "mutator", "mutators", "onboarding", "outdent", "overridable", "passthrough",
    "prefetch", "prefetched", "prefetching", "pseudocode", "quickstart", "rerender", "rerendered",
    "rerendering", "saas", "serverless", "signin", "stringify", "textarea", "timeseries",
    "tokenization", "tokenizes", "unencoded", "unindent", "uninit", "vectorize", "vectorized",
    "wireframe", "wireframes", "workaround", "workarounds", "writeline", "addon", "addons", "admin",
    "admins", "ajax", "alloc", "allocator", "allocators", "allocs", "allowlist", "api", "apis",
    "app", "apps", "argmax", "argmin", "arr", "arrs", "asm", "auth", "authn", "authz",
    "autocomplete", "autocompletion", "autoformat", "autoload", "autosave", "avg", "backend",
    "backends", "backoff", "backport", "backports", "backslash", "backslashes", "backtick",
    "backticks", "backtrace", "backtraces", "backtrack", "backtracking", "basename", "bitfield",
    "bitmask", "bitmasks", "bitwise", "blob", "blobs", "boolean", "booleans", "boilerplate",
    "breakpoint", "breakpoints", "btn", "btree", "buf", "bufs", "bugfix", "bugfixes", "callable",
    "callables", "callback", "callbacks", "cfg", "changelog", "changelogs", "charset", "charsets",
    "chdir", "checkbox", "checkboxes", "checksum", "checksums", "chmod", "chown", "cli", "clis",
    "cmd", "cmds", "cmp", "cnt", "codebase", "codebases", "codegen", "codepoint", "codepoints",
    "combobox", "comboboxes", "config", "configs", "configurable", "conn", "conns", "const",
    "consts", "cpu", "cpus", "cron", "crontab", "crud", "ctor", "ctors", "ctx", "cur", "curr",
    "cwd", "daemon", "daemons", "dataframe", "dataframes", "dataset", "datasets", "datastore", "db",
    "dbs", "dealloc", "debounce", "debuggable", "debugger", "debuggers", "decl", "decls", "decrypt",
    "decrypted", "dedent", "dedented", "dedup", "dedupe", "deduplicate", "deduplicated",
    "deduplication", "deinit", "delim", "delims", "denylist", "deque", "deques", "dequeue",
    "dequeued", "deref", "deserialization", "deserialize", "deserialized", "deserializer",
    "deserializers", "deserializing", "dest", "dev", "devops", "devs", "diff", "diffs", "dir",
    "dirname", "dirnames", "dirs", "dll", "dlls", "doc", "docs", "dom", "dropdown", "dropdowns",
    "dst", "dtor", "dtype", "dtypes", "dylib", "elem", "elems", "email", "emails", "endian",
    "endianness", "endpoint", "endpoints", "enqueue", "enqueued", "env", "envs", "epoch", "err",
    "errno", "errs", "eslint", "eval", "evals", "exe", "exes", "ext", "exts", "failover", "favicon",
    "fd", "fds", "fifo", "filename", "filenames", "filepath", "filepaths", "filesystem",
    "filesystems", "fixme", "fmt", "fn", "fns", "frontend", "frontends", "fs", "fullscreen", "func",
    "funcs", "getenv", "getter", "getters", "git", "gitignore", "glob", "globbing", "gpu", "gpus",
    "grayscale", "greyscale", "gui", "guid", "guids", "guis", "hardcode", "hardcoded", "hardlink",
    "hashmap", "hashmaps", "hashset", "hashtable", "hdr", "hexdigest", "hostname", "hostnames",
    "hotkey", "hotkeys", "href", "hrefs", "hyperlink", "hyperlinks", "ide", "ides", "idx", "idxs",
    "iframe", "img", "imgs", "impl", "impls", "inbox", "incr", "indices", "info", "infos",
    "initializer", "initializers", "inline", "inlined", "inlining", "inode", "inodes", "int",
    "ints", "io", "ip", "ips", "iso", "iterator", "iterators", "keybinding", "keybindings",
    "keymap", "keymaps", "keypress", "keystroke", "keystrokes", "kubernetes", "lexer", "lexers",
    "lexing", "lhs", "lib", "libc", "libs", "linter", "linters", "linting", "localhost", "lockfile",
    "lockfiles", "logfile", "logfiles", "login", "logins", "logout", "lookahead", "lookbehind",
    "lru", "lst", "lstrip", "malloc", "markdown", "matcher", "matchers", "memoization", "memoize",
    "memoized", "metadata", "middleware", "middlewares", "minified", "minify", "misc", "mkdir",
    "mmap", "mock", "mocked", "mocking", "mocks", "msg", "msgs", "multiline", "multiprocessing",
    "multithreaded", "multithreading", "mutex", "mutexes", "mysql", "namespace", "namespaces",
    "nan", "nans", "navbar", "newline", "newlines", "nil", "nosql", "nullable", "nullptr", "num",
    "nums", "obj", "objs", "offline", "ok", "online", "opcode", "opcodes", "orm", "os", "param",
    "params", "parametrize", "parametrized", "parameterize", "parameterized", "parsers", "passwd",
    "pathname", "pathnames", "perf", "pid", "pids", "pixmap", "pkg", "pkgs", "plaintext", "plugin",
    "plugins", "popup", "popups", "postgres", "postgresql", "postprocess", "pragma", "prepend",
    "prepended", "prepending", "prepends", "preprocess", "preprocessed", "preprocessing",
    "preprocessor", "prev", "proc", "procs", "prod", "proto", "ptr", "ptrs", "pwd", "qa",
    "querystring", "ram", "readme", "readmes", "readonly", "realloc", "realpath", "rebase",
    "rebased", "rebasing", "recv", "redis", "ref", "refactor", "refactored", "refactoring",
    "refactorings", "refactors", "refs", "regex", "regexes", "regexp", "regexps", "relpath", "repl",
    "repo", "repos", "req", "reqs", "res", "resp", "restful", "ret", "rhs", "rmdir", "rstrip",
    "runtime", "runtimes", "sandbox", "sandboxed", "schemas", "scrollbar", "scrollbars", "sdk",
    "sdks", "semver", "sep", "serializable", "serialization", "serialize", "serialized",
    "serializer", "serializers", "serializing", "setter", "setters", "shader", "shaders", "sidebar",
    "sidebars", "signup", "src", "srcs", "stacktrace", "stacktraces", "startup", "statusbar",
    "stderr", "stdin", "stdio", "stdout", "stmt", "stmts", "struct", "structs", "stylesheet",
    "stylesheets", "subcommand", "subcommands", "subdir", "subdirectories", "subdirectory",
    "subdirs", "subfolder", "subfolders", "submodule", "submodules", "subpackage", "subpackages",
    "substring", "substrings", "subtree", "subtrees", "sudo", "symlink", "symlinked", "symlinks",
    "sync", "synced", "syncing", "sys", "tbody", "teardown", "templating", "testcase", "testcases",
    "textbox", "thead", "threadpool", "timestamp", "timestamps", "timezone", "timezones", "tmp",
    "tmpdir", "tmpfile", "todo", "todos", "tokenize", "tokenized", "tokenizer", "tokenizers",
    "tokenizing", "toolbar", "toolbars", "toolchain", "toolchains", "tooltip", "tooltips",
    "transpile", "transpiled", "transpiler", "trie", "tries", "tty", "tui", "typecheck",
    "typechecker", "typechecking", "typedef", "tz", "tzinfo", "ui", "uid", "uids", "uint",
    "uncomment", "uncommented", "unescape", "unescaped", "unhandled", "unicode", "uninstall",
    "uninstalled", "uninstalling", "unmarshal", "unpack", "unpacked", "unpacking", "unparse",
    "unpickle", "unregister", "unregistered", "unselect", "unserializable", "unsubscribe",
    "untrusted", "upsert", "usb", "username", "usernames", "userspace", "util", "utils", "ux",
    "val", "vals", "var", "vars", "vec", "vecs", "versioned", "versioning", "viewport", "viewports",
    "vm", "vms", "webhook", "webhooks", "websocket", "websockets", "whitelist", "blacklist",
    "whitespace", "whitespaces", "wiki", "wikis", "wildcard", "wildcards", "workflow", "workflows",
    "workspace", "workspaces", "www", "xxx",
    // Docstring markup (reST/Sphinx, Google, NumPy, Epytext) and data science
    "abbr", "astype", "autoclass", "autodoc", "autofunction", "automodule", "batchnorm",
    "codeblock", "colormap", "cuda", "cvar", "dataloader", "dotenv", "envvar", "envvars", "epytext",
    "groupby", "hyperparameter", "hyperparameters", "iloc", "ivar", "kbd", "kwtype",
    "literalinclude", "logits", "makefile", "meth", "ndarray", "ndarrays", "noindex", "numpydoc",
    "pipfile", "queryset", "querysets", "relu", "rfc", "rtype", "samp", "seealso", "softmax",
    "sourcecode", "subplot", "subplots", "testcode", "testoutput", "toctree", "urlpatterns",
    "vartype", "versionadded", "versionchanged", "viewset", "viewsets", "backprop",
    "backpropagation", "chatbot", "chatbots", "dataloaders", "embeddings", "finetune", "finetuned",
    "finetuning", "layernorm", "llm", "llms", "optim", "overfitting", "pretrained",
    // Placeholders and English abbreviations common in code
    "aka", "approx", "asap", "baz", "btw", "com", "eg", "etc", "foo", "foobar", "fyi", "ie", "imho",
    "imo", "ipsum", "lgtm", "lorem", "mins", "msec", "net", "nit", "nits", "nsec", "org", "pct",
    "quux", "qux", "secs", "spam", "tbd", "usec", "vs", "wip", "wrt", "xyzzy",
];

/// Python keywords (hard and soft), in ASCII order.
#[rustfmt::skip]
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "case", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return",
    "try", "type", "while", "with", "yield",
];

/// Names in `builtins` (Python 3.13), in ASCII order.
#[rustfmt::skip]
const PYTHON_BUILTINS: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException", "BaseExceptionGroup",
    "BlockingIOError", "BrokenPipeError", "BufferError", "BytesWarning", "ChildProcessError",
    "ConnectionAbortedError", "ConnectionError", "ConnectionRefusedError", "ConnectionResetError",
    "DeprecationWarning", "EOFError", "Ellipsis", "EncodingWarning", "EnvironmentError",
    "Exception", "ExceptionGroup", "False", "FileExistsError", "FileNotFoundError",
    "FloatingPointError", "FutureWarning", "GeneratorExit", "IOError", "ImportError",
    "ImportWarning", "IndentationError", "IndexError", "InterruptedError", "IsADirectoryError",
    "KeyError", "KeyboardInterrupt", "LookupError", "MemoryError", "ModuleNotFoundError",
    "NameError", "None", "NotADirectoryError", "NotImplemented", "NotImplementedError", "OSError",
    "OverflowError", "PendingDeprecationWarning", "PermissionError", "ProcessLookupError",
    "PythonFinalizationError", "RecursionError", "ReferenceError", "ResourceWarning",
    "RuntimeError", "RuntimeWarning", "StopAsyncIteration", "StopIteration", "SyntaxError",
    "SyntaxWarning", "SystemError", "SystemExit", "TabError", "TimeoutError", "True", "TypeError",
    "UnboundLocalError", "UnicodeDecodeError", "UnicodeEncodeError", "UnicodeError",
    "UnicodeTranslateError", "UnicodeWarning", "UserWarning", "ValueError", "Warning",
    "ZeroDivisionError", "abs", "aiter", "all", "anext", "any", "ascii", "bin", "bool",
    "breakpoint", "bytearray", "bytes", "callable", "chr", "classmethod", "compile", "complex",
    "copyright", "credits", "delattr", "dict", "dir", "divmod", "enumerate", "eval", "exec", "exit",
    "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr", "hash", "help",
    "hex", "id", "input", "int", "isinstance", "issubclass", "iter", "len", "license", "list",
    "locals", "map", "max", "memoryview", "min", "next", "object", "oct", "open", "ord", "pow",
    "print", "property", "quit", "range", "repr", "reversed", "round", "set", "setattr", "slice",
    "sorted", "staticmethod", "str", "sum", "super", "tuple", "type", "vars", "zip",
];

/// Comment prefixes (lowercase, after `#` and spaces) of tool directives whose
/// text is not prose: `# type: ignore[...]`, `# noqa: E501`, `# fmt: off`, …
#[rustfmt::skip]
const COMMENT_DIRECTIVES: &[&str] = &[
    "-*-", "autopep8:", "black:", "codespell:", "coverage:", "cspell:", "flake8:", "fmt:", "isort:",
    "language=", "mypy:", "noinspection", "noqa", "nosec", "pragma:", "pycln:", "pydocstyle:",
    "pylint:", "pyre-", "pyright:", "pytype:", "ruff:", "spell-checker:", "spellchecker:", "type:",
    "vi:", "vim:", "yapf:",
];

/// Jargon plus the lowercase builtins and keywords: words that are always
/// accepted, sorted for binary search.
pub(crate) fn extra_words() -> &'static [&'static str] {
    static EXTRA: OnceLock<Vec<&'static str>> = OnceLock::new();
    EXTRA.get_or_init(|| {
        let lowercase_names = PYTHON_KEYWORDS
            .iter()
            .chain(PYTHON_BUILTINS)
            .filter(|w| w.bytes().all(|b| b.is_ascii_lowercase()));
        let mut words: Vec<&'static str> = PROGRAMMING_JARGON
            .iter()
            .chain(lowercase_names)
            .copied()
            .collect();
        words.sort_unstable();
        words.dedup();
        words
    })
}

/// True for programming jargon and lowercase Python names (`kwargs`, `len`,
/// `isinstance`, `nonlocal`); `lower` must already be lowercase.
pub(crate) fn is_known_word(lower: &str) -> bool {
    extra_words().binary_search(&lower).is_ok()
}

/// True for Python keywords and builtins, compared case-sensitively
/// (`ZeroDivisionError`, `isinstance`, `nonlocal`, `None`).
pub(crate) fn is_python_name(name: &str) -> bool {
    PYTHON_KEYWORDS.binary_search(&name).is_ok() || PYTHON_BUILTINS.binary_search(&name).is_ok()
}

/// True for dunder names: `__init__`, `__post_init__`, `__all__`.
pub(crate) fn is_dunder(name: &str) -> bool {
    name.len() > 4
        && name.starts_with("__")
        && name.ends_with("__")
        && name[2..name.len() - 2].bytes().any(|b| b != b'_')
}

/// True when a comment (its text after `#`) is a tool directive. Only the
/// start of `comment` is looked at, so this is cheap on long comments.
pub(crate) fn is_directive(comment: &str) -> bool {
    let text = comment.trim_start();
    // Every directive starts with a letter or `-`.
    let candidate = text
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'-');
    candidate
        && COMMENT_DIRECTIVES.iter().any(|d| {
            text.get(..d.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(d))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_well_formed() {
        for w in PROGRAMMING_JARGON {
            assert!(
                w.len() >= 2 && w.bytes().all(|b| b.is_ascii_lowercase()),
                "bad jargon entry {w:?}"
            );
        }
        for list in [PYTHON_KEYWORDS, PYTHON_BUILTINS, COMMENT_DIRECTIVES] {
            for pair in list.windows(2) {
                assert!(pair[0] < pair[1], "not sorted: {pair:?}");
            }
        }
        let extra = extra_words();
        assert!(extra.windows(2).all(|p| p[0] < p[1]));
    }

    #[test]
    fn known_words_and_python_names() {
        for w in [
            "args",
            "kwargs",
            "len",
            "str",
            "bool",
            "async",
            "utf",
            "json",
            "http",
            "api",
            "uuid",
            "regex",
            "tmp",
            "config",
            "init",
            "repr",
            "iter",
            "pyi",
            "dataclass",
            "typeshed",
            "isinstance",
            "nonlocal",
            "zip",
            "elif",
        ] {
            assert!(is_known_word(w), "{w}");
        }
        assert!(!is_known_word("recieve"));
        assert!(!is_known_word("Kwargs"));
        assert!(is_python_name("ZeroDivisionError"));
        assert!(is_python_name("isinstance"));
        assert!(is_python_name("None"));
        assert!(is_python_name("nonlocal"));
        assert!(!is_python_name("none"));
        assert!(!is_python_name("Isinstance"));
    }

    #[test]
    fn dunders_and_directives() {
        assert!(is_dunder("__init__"));
        assert!(is_dunder("__post_init__"));
        assert!(is_dunder("__all__"));
        assert!(!is_dunder("____"));
        assert!(!is_dunder("_____"));
        assert!(!is_dunder("__private"));
        assert!(!is_dunder("trailing__"));
        assert!(is_directive(" type: ignore[attr-defined]"));
        assert!(is_directive(" noqa: E501"));
        assert!(is_directive("  fmt: off"));
        assert!(is_directive(" -*- coding: utf-8 -*-"));
        assert!(is_directive(" PRAGMA: no cover"));
        assert!(!is_directive(" the type of the value"));
    }
}
