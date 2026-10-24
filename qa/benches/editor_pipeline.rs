use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rustpy_format::{format_source, FormatOptions};
fn pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_pipeline");
    // Deliberately bounded; the million-line workload is a separate final gate.
    for count in [10usize, 100, 1000] {
        let source = "def compute(value: int) -> int:\n    return value + 1\n\n".repeat(count);
        group.bench_with_input(BenchmarkId::new("lexer", count), &source, |b, s| {
            b.iter(|| rustpy_syntax::lexer::tokenize(black_box(s)))
        });
        group.bench_with_input(BenchmarkId::new("parser", count), &source, |b, s| {
            b.iter(|| rustpy_syntax::parse(black_box(s)))
        });
        group.bench_with_input(BenchmarkId::new("formatter", count), &source, |b, s| {
            b.iter(|| format_source(black_box(s), &FormatOptions::default()))
        });
    }
    group.finish();
}
criterion_group!(benches, pipeline);
criterion_main!(benches);
