"""Authored UNEXECUTED regression evidence; no subprocesses or corpus generation."""
import copy
import unittest
from acceptance import evaluate, percentile


def native_trace():
    run = dict(app_pid=40,app_pid_verified=True,app_executable_sha256="frozen-sha",binary_sha256="frozen-sha",app_start_epoch_ns=1_000_000_000,start_epoch_ns=500_000_000,returncode=0,timed_out=False,gui_route="scripts/smoke.sh",
               renderer="software",stage="native10k",peak_app_rss_bytes=800_000_000,peak_app_tree_rss_bytes=900_000_000,peak_tree_rss_bytes=950_000_000,
               watcher_path="/fixture/external.py",watcher_write_epoch_ns=5_000_000_000,
               script_steps=["start","typing","power-save-on","power-save-off","invalidate-caches","invalidate-caches-apply","end"])
    rows = []
    def add(kind,data,ns=2_000_000_000):
        rows.append(dict(schema=1,pid=40,epoch_ns=ns,kind=kind,data=data))
    add("first_frame",{})
    for label in run["script_steps"]:
        add("script_step",dict(label=label))
    add("renderer",dict(renderer="llvmpipe (LLVM 18.1)",vendor="Mesa"))
    add("index",dict(busy=True,files=0,generation=0))
    add("frame",dict(ms=5,input=False))
    add("viewport",dict(first=0,last=42,laid_out=42,total_rows=5001,document_lines=5001,
                        clip_height=800,line_height=20,large_file=False))
    add("index",dict(busy=False,files=10_001,generation=1))
    for _ in range(20):
        add("typing_frame",dict(ms=8,document_lines=5001,large_file=False))
        add("completion_popup",dict(ms=40,items=100))
    add("mode",dict(power_save=True,indexing=False))
    add("mode",dict(power_save=False,indexing=False))
    add("caches_invalidated",dict(analysis_replaced=True,index_rebuild_requested=True,caches=["filesystem"]))
    add("watcher_ready",dict(recursive=True))
    add("watcher_event",dict(paths=["/fixture/external.py"],rescan=False),5_200_000_000)
    add("analysis_cancel",dict(id=1),6_000_000_000)
    add("analysis_end",dict(id=1,cancelled=True),6_010_000_000)
    return rows,run


class NativeAcceptanceContracts(unittest.TestCase):
    def test_complete_evidence_is_stage_only(self):
        rows,run=native_trace()
        result=evaluate(rows,run)
        self.assertEqual(result["status"],"pass",result)
        self.assertIn("one stage only",result["scope"])

    def test_undersized_project_and_component_only_input_cannot_pass(self):
        rows,run=native_trace()
        for row in rows:
            if row["kind"] == "index": row["data"]["files"]=1000
        self.assertEqual(evaluate(rows,run)["status"],"fail")
        rows,run=native_trace()
        rows=[r for r in rows if r["kind"] != "typing_frame"]
        self.assertEqual(evaluate(rows,run)["status"],"fail")

    def test_cancel_request_alone_and_stale_worker_end_do_not_measure_quiescence(self):
        for drop,epoch in ((True,0),(False,5_000_000_000)):
            rows,run=native_trace()
            if drop: rows=[r for r in rows if r["kind"] != "analysis_end"]
            else:
                next(r for r in rows if r["kind"] == "analysis_end")["epoch_ns"]=epoch
            checks=evaluate(rows,run)["checks"]
            self.assertFalse(next(c for c in checks if c["name"] == "cancellation_p95_ms")["passed"])

    def test_boundaries_are_strict_and_backend_is_observed(self):
        for field,value in (("peak_app_tree_rss_bytes",1_000_000_000),("renderer","gpu"),("timed_out",True)):
            rows,run=native_trace();run[field]=value
            self.assertEqual(evaluate(rows,run)["status"],"fail")
        rows,run=native_trace()
        next(r for r in rows if r["kind"] == "first_frame")["epoch_ns"]=3_000_000_000
        self.assertEqual(evaluate(rows,run)["status"],"fail")

    def test_foreign_pid_wrong_sentinel_and_nonvirtualized_layout_are_rejected(self):
        rows,run=native_trace()
        for mutation in ("pid","sentinel","viewport"):
            altered=copy.deepcopy(rows)
            if mutation=="pid": altered[0]["pid"]=99
            elif mutation=="sentinel": next(r for r in altered if r["kind"]=="watcher_event")["data"]["paths"]=["/fixture/other.py"]
            else: next(r for r in altered if r["kind"]=="viewport")["data"]["laid_out"]=5001
            self.assertEqual(evaluate(altered,run)["status"],"fail")

    def test_watcher_limit_failures_and_power_save_background_work_are_not_hidden(self):
        rows,run=native_trace()
        event=dict(schema=1,pid=40,epoch_ns=2_000_000_000,kind="watcher_unavailable",data={"error":"inotify limit reached"})
        self.assertEqual(evaluate(rows+[event],run)["status"],"fail")
        on=next(i for i,r in enumerate(rows) if r["kind"]=="mode" and r["data"]["power_save"])
        rows.insert(on+1,dict(event,kind="analysis_start",data={"id":3}))
        self.assertEqual(evaluate(rows,run)["status"],"fail")

    def test_invalid_or_empty_distribution_is_not_a_zero_latency_success(self):
        for values in ([],[float("nan")],[-1],[float("inf")]):
            with self.assertRaises(ValueError): percentile(values)

    def test_modal_or_large_file_plain_text_samples_do_not_satisfy_typing(self):
        for updates in ({"document_lines":12},{"large_file":True}):
            rows,run=native_trace()
            for row in rows:
                if row["kind"] == "typing_frame": row["data"].update(updates)
            checks=evaluate(rows,run)["checks"]
            self.assertFalse(next(c for c in checks if c["name"] == "typing_document")["passed"])


    def test_wrapper_rss_cannot_substitute_for_missing_application_identity(self):
        rows,run=native_trace()
        for mutation in ({"app_pid_verified":False},{"app_executable_sha256":"other"},{"peak_app_rss_bytes":0},{"gui_route":"direct-display"}):
            changed=dict(run,**mutation)
            self.assertEqual(evaluate(rows,changed)["status"],"fail")

    def test_cold_start_uses_application_birth_while_retaining_wrapper_start(self):
        rows,run=native_trace()
        run["start_epoch_ns"] = 1
        self.assertEqual(evaluate(rows,run)["status"],"pass")

    def test_unreached_script_steps_fail_explicitly(self):
        rows,run=native_trace()
        rows=[r for r in rows if not (r["kind"]=="script_step" and r["data"]["label"] in ("invalidate-caches","end"))]
        result=evaluate(rows,run)
        self.assertEqual(result["status"],"fail")
        checks={c["name"]:c for c in result["checks"]}
        self.assertEqual(checks["script_steps_reached"]["observed"],{"not reached":["invalidate-caches","end"]})
        self.assertEqual(checks["invalidate_caches"]["observed"],"not reached")
        self.assertFalse(checks["invalidate_caches"]["passed"])
        self.assertTrue(checks["power_save_suppresses_analysis"]["passed"])

    def test_invalidate_caches_needs_the_dialog_apply_step_and_its_trace(self):
        rows,run=native_trace()
        self.assertTrue({c["name"]:c for c in evaluate(rows,run)["checks"]}["invalidate_caches"]["passed"])
        # The dialog opened but its default button was never confirmed.
        unapplied=[r for r in rows if not (r["kind"]=="script_step" and r["data"]["label"]=="invalidate-caches-apply")]
        unapplied=[r for r in unapplied if r["kind"]!="caches_invalidated"]
        check={c["name"]:c for c in evaluate(unapplied,run)["checks"]}["invalidate_caches"]
        self.assertEqual(check["observed"],"not reached")
        self.assertFalse(check["passed"])
        # Applied without the file system/analysis caches selected.
        partial=[dict(r,data=dict(r["data"],caches=["git_log"])) if r["kind"]=="caches_invalidated" else r for r in rows]
        self.assertFalse({c["name"]:c for c in evaluate(partial,run)["checks"]}["invalidate_caches"]["passed"])

    def test_truncated_trace_fails_and_cap_is_configurable(self):
        rows,run=native_trace()
        run["trace_max_rows"]=len(rows)
        self.assertEqual(evaluate(rows,run)["status"],"fail")
        run["trace_max_rows"]=len(rows)+1
        self.assertEqual(evaluate(rows,run)["status"],"pass")
        rows.append(dict(schema=1,pid=40,epoch_ns=7_000_000_000,kind="trace_truncated",data={"max_rows":10}))
        run["trace_max_rows"]=1_000_000
        self.assertEqual(evaluate(rows,run)["status"],"fail")

    def test_count_only_frames_still_measure_background_indexing(self):
        rows,run=native_trace()
        rows=[r for r in rows if r["kind"]!="frame"]
        checks=lambda: {c["name"]:c for c in evaluate(rows,run)["checks"]}
        self.assertFalse(checks()["background_indexing"]["passed"])
        indexes=[r for r in rows if r["kind"]=="index"]
        indexes[0]["data"]["frames"]=3
        indexes[1]["data"]["frames"]=40
        self.assertEqual(checks()["background_indexing"]["observed"],37)
        self.assertEqual(evaluate(rows,run)["status"],"pass")
