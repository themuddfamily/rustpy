use super::*;

#[test]
fn parentheses_do_not_restore_replaced_no_return_member_authority() {
    for call in ["(c.stop)()", "((c.stop))()", "((c.stop)())"] {
        let source = format!(
            "from typing import NoReturn\nclass C:\n    def stop(self) -> NoReturn:\n        raise RuntimeError\ndef f(flag):\n    c = C()\n    c.stop = lambda: None\n    if flag:\n        value = 1\n    else:\n        {call}\n    return value\n"
        );
        let names: Vec<_> = found(&source, &run(&source), "PyUnboundLocalVariable")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["value"], "{source}");
    }
}

#[test]
fn parentheses_preserve_genuine_terminal_call_contracts() {
    for source in [
        "def f(flag):\n    if flag:\n        value = 1\n    else:\n        ((exit()))\n    return value\n",
        "import sys\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        (sys.exit)(1)\n    return value\n",
        "from typing import NoReturn\ndef stop() -> NoReturn:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        ((stop()))\n        pass\n    return value\n",
    ] {
        assert!(
            found(source, &run(source), "PyUnboundLocalVariable").is_empty(),
            "{source}"
        );
    }
}

#[test]
fn bare_attribute_annotations_do_not_replace_genuine_never_contracts() {
    for source in [
        "from typing import NoReturn\nclass C:\n    def stop(self) -> NoReturn:\n        raise RuntimeError\ndef f(flag):\n    c = C()\n    c.stop: object\n    if flag:\n        value = 1\n    else:\n        c.stop()\n        pass\n    return value\n",
        "import typing\ntyping.NoReturn: object\ndef stop() -> typing.NoReturn:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n        pass\n    return value\n",
    ] {
        assert!(
            found(source, &run(source), "PyUnboundLocalVariable").is_empty(),
            "{source}"
        );
    }
}
