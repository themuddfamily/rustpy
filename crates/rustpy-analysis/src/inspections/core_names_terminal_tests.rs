use super::*;

fn reports_value(source: &str) -> Vec<String> {
    found(source, &run(source), "PyUnboundLocalVariable")
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn shadowed_terminal_names_preserve_reachable_unbound_tail() {
    for (parameters, call) in [
        ("exit", "exit()"),
        ("quit", "quit()"),
        ("sys", "sys.exit()"),
        ("os", "os._exit(0)"),
        ("os", "os.abort()"),
        ("typing", "typing.assert_never(None)"),
        ("typing_extensions", "typing_extensions.assert_never(None)"),
        ("pytest", "pytest.fail()"),
        ("pytest", "pytest.skip()"),
        ("pytest", "pytest.exit()"),
        ("pytest", "pytest.xfail()"),
        ("assert_never", "assert_never(None)"),
    ] {
        let source = format!(
            "def f(flag, {parameters}):\n    if flag:\n        value = 1\n    else:\n        {call}\n    return value\n"
        );
        assert_eq!(reports_value(&source), ["value"], "{source}");
    }
}
#[test]
fn rebound_import_and_outer_names_preserve_reachable_unbound_tail() {
    for source in [
        "def f(flag):\n    exit = lambda: None\n    if flag:\n        value = 1\n    else:\n        exit()\n    return value\n",
        "def f(flag, fake):\n    import sys\n    sys = fake\n    if flag:\n        value = 1\n    else:\n        sys.exit()\n    return value\n",
        "exit = lambda: None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        exit()\n    return value\n",
        "class Case:\n    def fail(self):\n        pass\n    def f(self, flag):\n        if flag:\n            value = 1\n        else:\n            self.fail()\n        return value\n",
        "class Case:\n    def skipTest(self, reason):\n        pass\n    def f(self, flag):\n        if flag:\n            value = 1\n        else:\n            self.skipTest('owned')\n        return value\n",
    ] {
        assert_eq!(reports_value(source), ["value"], "{source}");
    }
}
#[test]
fn canonical_termination_and_real_no_return_keep_defined_tail() {
    for source in [
        "def f(flag):\n    if flag:\n        value = 1\n    else:\n        exit()\n    return value\n",
        "import sys\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        sys.exit(1)\n    return value\n",
        "import os\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        os._exit(1)\n    return value\n",
        "from typing import NoReturn\ndef stop() -> NoReturn:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n    return value\n",
        "def stop():\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n    return value\n",
    ] {
        assert!(reports_value(source).is_empty(), "{source}");
    }
}
