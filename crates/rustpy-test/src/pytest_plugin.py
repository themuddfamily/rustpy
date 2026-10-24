"""Importable pytest plugin, loaded in controller and xdist workers alike."""
import json
import os
import pytest
from rustpy_test_reporter import PytestReporter, count_assert_explanations, install_equality_capture


def _hypothesis_failure_text(item, report):
    text = getattr(report, "longreprtext", "")
    # Upstream changed the minimized-example heading. Require the actual
    # decorated-test marker so an unrelated assertion mentioning it cannot
    # acquire Hypothesis metadata merely through its traceback text.
    if getattr(getattr(item, "obj", None), "is_hypothesis_test", False) and any(
        heading in text for heading in ("Falsifying example", "Failing test case:", "Hypothesis")
    ):
        return text
    return ""


def pytest_configure(config):
    # xdist workers import the plugin without executing reporter.main(). Keep
    # their unittest comparison capture on this same canonical module too.
    install_equality_capture()
    reporter = PytestReporter(os.environ.get("RUSTPY_TEST_ROOT", str(config.rootpath)))
    reporter.worker = os.environ.get("PYTEST_XDIST_WORKER")
    config.pluginmanager.register(reporter, "rustpy-json-reporter")
    count_assert_explanations(reporter)


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    # user_properties are transported by xdist; never depend on worker stdout.
    metadata = dict(id=item.nodeid, logs=getattr(report, "caplog", "") or "",
                    worker=os.environ.get("PYTEST_XDIST_WORKER"),
                    hypothesis="\n".join(str(content) for name, content in getattr(report, "sections", []) if "hypothesis" in name.lower()))
    if not metadata["hypothesis"]:
        metadata["hypothesis"] = _hypothesis_failure_text(item, report)
    fixtures = []
    for name, definitions in getattr(getattr(item, "_fixtureinfo", None), "name2fixturedefs", {}).items():
        if definitions:
            function = getattr(definitions[-1], "func", None)
            code = getattr(function, "__code__", None)
            if code:
                fixtures.append((name, code.co_filename, code.co_firstlineno))
    metadata.update(fixtures=fixtures, markers=[marker.name for marker in item.iter_markers()],
                    parameter_id=getattr(getattr(item, "callspec", None), "id", None))
    report.user_properties.append(("rustpy_metadata", json.dumps(metadata, default=str)))
    reporter = item.config.pluginmanager.get_plugin("rustpy-json-reporter")
    comparison = reporter._comparison() if reporter and call.excinfo else None
    if comparison:
        report.user_properties.append(("rustpy_comparison", comparison))
