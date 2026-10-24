"""Strict evidence evaluator for native RustPy measurements; no GUI imports."""
import math

LIMITS = {"cold_start_ms": 2000, "typing_p95_ms": 16, "completion_p95_ms": 100,
          "peak_tree_rss_bytes": 1_000_000_000, "cancellation_p95_ms": 100,
          "watcher_ms": 2000}


def percentile(values, fraction=.95):
    if not values or any(not math.isfinite(v) or v < 0 for v in values):
        raise ValueError("missing or invalid measurements")
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


def evaluate(rows, run):
    """Fail closed: absent, truncated, undersized or foreign-PID traces cannot pass.

    Native frame/input timings include highlighting and layout, but end before
    the GPU presents; presentation latency requires native platform evidence.
    """
    checks = []
    def check(name, ok, observed=None, required=None):
        checks.append(dict(name=name, passed=bool(ok), observed=observed, required=required))
    valid = [r for r in rows if r.get("schema") == 1 and r.get("pid") == run.get("app_pid")]
    check("trace_identity", len(valid) == len(rows) and bool(valid))
    def data(kind):
        return [r["data"] for r in valid if r.get("kind") == kind]
    max_rows = run.get("trace_max_rows", 100_000)
    truncated = bool(data("trace_truncated"))
    check("bounded_trace", not truncated and len(rows) < max_rows, {"rows":len(rows),"truncated":truncated}, f"<{max_rows} rows, no trace_truncated row")
    # `mark` steps of the input script: a step that never ran is "not
    # reached" and fails, instead of being silently absent evidence.
    reached = {v.get("label") for v in data("script_step")}
    expected_steps = run.get("script_steps", [])
    missing = [step for step in expected_steps if step not in reached]
    check("script_steps_reached", not missing, {"not reached":missing} if missing else "all reached", expected_steps)
    def gated(step, ok, observed=None):
        """A check whose script step never ran reports "not reached"."""
        if step in expected_steps and step not in reached:
            return False, "not reached"
        return ok, observed
    check("app_process_identity", run.get("app_pid_verified") and run.get("app_executable_sha256") == run.get("binary_sha256") and bool(run.get("binary_sha256")))
    first = next((r["epoch_ns"] for r in valid if r.get("kind") == "first_frame"), None)
    cold = (first-run["app_start_epoch_ns"])/1e6 if first is not None and run.get("app_start_epoch_ns") is not None else None
    check("cold_start_ms", cold is not None and 0 <= cold < 2000, cold, "<2000; verified RustPy process birth to first completed UI frame (wrapper/Xvfb startup excluded)")
    check("normal_exit", run.get("returncode") == 0 and not run.get("timed_out") and not run.get("memory_limit_exceeded"))
    check("gui_route", run.get("gui_route") == "scripts/smoke.sh")
    viewports = data("viewport")
    check("virtualization", bool(viewports) and all(0 <= v["first"] <= v["last"] <= v["total_rows"] and v["laid_out"] <= v["last"]-v["first"] and v["last"]-v["first"] <= math.ceil(v["clip_height"]/v["line_height"])+3 for v in viewports), len(viewports), "visible rows plus <=3 overscan")
    renderer = data("renderer")
    software_names = ("llvmpipe", "softpipe", "swrast", "software rasterizer", "gdi generic", "swiftshader")
    actual = renderer[-1].get("renderer", "") if renderer else ""
    software = any(name in actual.lower() for name in software_names)
    check("actual_renderer", bool(actual) and (software if run["renderer"] == "software" else not software), actual, run["renderer"])
    if run["stage"] in ("native10k", "memory100k"):
        expected = 10_000 if run["stage"] == "native10k" else 100_000
        indexes = data("index")
        check("full_project_index", any(not v["busy"] and v["files"] >= expected for v in indexes), max((v["files"] for v in indexes), default=0), expected)
        pending = False
        frames_during_index = 0
        counted_during_index = 0
        busy_frames = None
        for row in valid:
            if row.get("kind") == "index":
                # Index rows carry the total completed frames, so busy
                # intervals stay measurable when `frame` rows are count-only.
                frames = row["data"].get("frames")
                if pending and busy_frames is not None and frames is not None:
                    counted_during_index += max(0, frames-busy_frames)
                pending = row["data"]["busy"]
                busy_frames = frames if pending else None
            elif pending and row.get("kind") == "frame": frames_during_index += 1
        frames_during_index = max(frames_during_index, counted_during_index)
        check("background_indexing", frames_during_index > 0, frames_during_index, "completed native UI frames while index busy")
        modes = [r for r in valid if r.get("kind") == "mode"]
        power = False
        saw_power = False
        power_violation = False
        for row in valid:
            if row.get("kind") == "mode":
                power = row["data"]["power_save"]
                saw_power |= power
            elif power and row.get("kind") == "analysis_start" and not row["data"].get("cancelled",False): power_violation = True
        first_power = next((i for i,r in enumerate(modes) if r["data"]["power_save"]), None)
        resumed = first_power is not None and any(not r["data"]["power_save"] for r in modes[first_power+1:])
        ok, observed = gated("power-save-off", saw_power and resumed and not power_violation)
        ok, observed = gated("power-save-on", ok, observed)
        check("power_save_suppresses_analysis", ok, observed)
        invalidated = data("caches_invalidated")
        ok, observed = gated("invalidate-caches-apply", any("filesystem" in v.get("caches", ["filesystem"]) for v in invalidated), invalidated[-1] if invalidated else None)
        check("invalidate_caches", *gated("invalidate-caches", ok, observed))
        check("watcher_coverage", bool(data("watcher_ready")) and not data("watcher_unavailable"))
        for kind, name, limit in (("typing_frame","typing_p95_ms",16),("completion_popup","completion_p95_ms",100)):
            values = [v["ms"] for v in data(kind)]
            p95 = percentile(values) if values else None
            check(name, *gated("typing", len(values) >= 20 and p95 < limit, {"samples":len(values),"p95":p95}), f">=20 samples, p95<{limit}")
        check("typing_document", bool(data("typing_frame")) and all(v.get("document_lines",0) >= 5000 and not v.get("large_file",True) for v in data("typing_frame")), None, "focused non-large 5k-line editor; excludes modal typing and plain-text fallback")
        check("peak_app_tree_rss_bytes", 0 < run.get("peak_app_rss_bytes",0) <= run.get("peak_app_tree_rss_bytes",0) < LIMITS["peak_tree_rss_bytes"], {"app":run.get("peak_app_rss_bytes"),"app_tree":run.get("peak_app_tree_rss_bytes"),"owned_tree_safety":run.get("peak_tree_rss_bytes")}, "<1000000000; verified RustPy plus descendants, excluding wrapper/Xvfb")
        sent = run.get("watcher_write_epoch_ns")
        deliveries = [r["epoch_ns"] for r in valid if r.get("kind") == "watcher_event" and any(p == run.get("watcher_path") for p in r["data"].get("paths", [])) and sent is not None and r["epoch_ns"] >= sent]
        latency = (min(deliveries)-sent)/1e6 if deliveries else None
        check("watcher_ms", latency is not None and latency < 2000, latency, "exact external sentinel delivery <2000ms")
        cancels = {r["data"]["id"]:r["epoch_ns"] for r in valid if r.get("kind") == "analysis_cancel" and r["data"]["id"] > 0}
        latencies = [(r["epoch_ns"]-cancels[r["data"]["id"]])/1e6 for r in valid if r.get("kind") == "analysis_end" and r["data"].get("cancelled") and r["data"]["id"] in cancels and r["epoch_ns"] >= cancels[r["data"]["id"]]]
        p95 = percentile(latencies) if latencies else None
        check("cancellation_p95_ms", bool(latencies) and p95 < 100, {"samples":len(latencies),"p95":p95}, "actual superseded worker completion <100ms; not just cancel() return")
    if run["stage"] == "largefile":
        check("large_file_mode", any(v["large_file"] and v["document_lines"] >= 100_000 for v in viewports))
    return {"schema":1,"scope":"one stage only; not whole roadmap acceptance","status":"pass" if all(c["passed"] for c in checks) else "fail", "checks":checks,
            "boundaries":["UI completion/input timings end at completed layout, not display presentation", "Linux application RSS and app-tree RSS exclude Xvfb/wrappers; whole owned tree is a separate safety watchdog", "Cold means fresh verified RustPy process and isolated system index directory; kernel page cache is not forcibly dropped", "Unchanged smoke.sh is software-only; hardware GPU acceptance remains unavailable"]}
