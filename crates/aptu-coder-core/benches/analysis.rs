// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use aptu_coder_core::cache::{CallGraphCache, CallGraphCacheKey};
use aptu_coder_core::graph::StructuralGraph;
use aptu_coder_core::types::SymbolMatchMode;
use blake3;
use criterion::{Criterion, criterion_group, criterion_main};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio_util::sync::CancellationToken;

fn overview_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("overview");
    group.sample_size(10);

    group.bench_function("analyze_directory_src", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("src"));
            let entries = aptu_coder_core::traversal::walk_directory(path, None).unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    group.finish();
}

fn file_details_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_details");
    group.sample_size(10);

    group.bench_function("analyze_file_lib_rs", |b| {
        b.iter(|| {
            let path = std::hint::black_box("src/lib.rs");
            let ast_recursion_limit = std::hint::black_box(None);

            aptu_coder_core::analyze::analyze_file(path, ast_recursion_limit)
        });
    });

    group.finish();
}

fn symbol_focus_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("symbol_focus");
    group.sample_size(10);

    group.bench_function("analyze_focused_src", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("src"));
            let focus = std::hint::black_box("analyze_directory".to_string());
            let follow_depth = std::hint::black_box(2);
            let max_depth = std::hint::black_box(None);
            let ast_recursion_limit = std::hint::black_box(None);
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            let params = aptu_coder_core::analyze::FocusedAnalysisConfig {
                focus,
                match_mode: SymbolMatchMode::Exact,
                follow_depth,
                max_depth,
                ast_recursion_limit,
                use_summary: false,
                impl_only: None,
                def_use: false,
                parse_timeout_micros: None,
            };

            aptu_coder_core::analyze::analyze_focused_with_progress(path, &params, progress, ct)
        });
    });

    group.finish();
}

fn subtree_count_overhead(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: root/ with 3 levels and 120 files (5 * 4 * 6)
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..4usize {
            let subsub = root.join(format!("sub{}", i)).join(format!("subsub{}", j));
            fs::create_dir_all(&subsub).unwrap();
            for k in 0..6usize {
                fs::write(subsub.join(format!("file{}.rs", k)), b"fn main() {}").unwrap();
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead");
    group.sample_size(10);

    group.bench_function("baseline_walk_only", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory; filter for bounded subset.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            let bounded: Vec<_> = all_entries.into_iter().filter(|e| e.depth <= 2).collect();
            std::hint::black_box((bounded, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn subtree_count_overhead_500(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: 3 directory levels deep; files sit at depth 4 (root=0, sub=1, subsub=2, subsubsub=3, file=4).
    // Total: 5 * 5 * 4 * 5 = 500 files.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..5usize {
            for k in 0..4usize {
                let subdir = root
                    .join(format!("sub{}", i))
                    .join(format!("subsub{}", j))
                    .join(format!("subsubsub{}", k));
                fs::create_dir_all(&subdir).unwrap();
                for m in 0..5usize {
                    fs::write(subdir.join(format!("file{}.rs", m)), b"fn main() {}").unwrap();
                }
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead_500");
    group.sample_size(10);

    group.bench_function("baseline_walk_only", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory.
            // Both this and baseline_walk_only do an unbounded walk; the only difference is the counting step.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            std::hint::black_box((all_entries, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn subtree_count_overhead_1000(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: 5 * 5 * 5 * 8 = 1000 files.
    // Directory structure: root/sub{0-4}/subsub{0-4}/subsubsub{0-4}/file{0-7}.rs
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..5usize {
            for k in 0..5usize {
                let subdir = root
                    .join(format!("sub{}", i))
                    .join(format!("subsub{}", j))
                    .join(format!("subsubsub{}", k));
                fs::create_dir_all(&subdir).unwrap();
                for m in 0..8usize {
                    fs::write(subdir.join(format!("file{}.rs", m)), b"fn main() {}").unwrap();
                }
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead_1000");
    group.sample_size(10);

    group.bench_function("baseline_walk_only_1000", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count_1000", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            std::hint::black_box((all_entries, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn analyze_module_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze_module");
    group.sample_size(10);

    group.bench_function("analyze_module_file_lib_rs", |b| {
        b.iter(|| {
            let path = std::hint::black_box("src/lib.rs");
            aptu_coder_core::analyze::analyze_module_file(path)
        });
    });

    group.finish();
}

fn analyze_directory_depth_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze_directory_depth");
    group.sample_size(10);

    // Benchmark max_depth=1 at repo root (sync walker path)
    group.bench_function("depth_1_repo_root", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("."));
            let entries =
                aptu_coder_core::traversal::walk_directory(path, std::hint::black_box(Some(1)))
                    .unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    // Benchmark max_depth=2 at repo root (parallel walker path for comparison)
    group.bench_function("depth_2_repo_root", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("."));
            let entries =
                aptu_coder_core::traversal::walk_directory(path, std::hint::black_box(Some(2)))
                    .unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    group.finish();
}

fn call_graph_cache_benchmark(c: &mut Criterion) {
    let root = Path::new("src");
    let entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();

    let params = aptu_coder_core::analyze::FocusedAnalysisConfig {
        focus: "analyze_directory".to_string(),
        match_mode: SymbolMatchMode::Exact,
        follow_depth: 2,
        max_depth: None,
        ast_recursion_limit: None,
        use_summary: false,
        impl_only: None,
        def_use: false,
        parse_timeout_micros: None,
    };

    // Pre-compute output and populate cache for warm_hit benchmark
    let precomputed_progress = Arc::new(AtomicUsize::new(0));
    let precomputed_ct = CancellationToken::new();
    let output = aptu_coder_core::analyze::analyze_focused_with_progress_with_entries(
        root,
        &params,
        &precomputed_progress,
        &precomputed_ct,
        &entries,
        None,
    )
    .unwrap();

    let key = CallGraphCacheKey::from_entries(
        root,
        &entries,
        None,
        params.follow_depth,
        &params.match_mode,
        params.impl_only.unwrap_or(false),
        params.ast_recursion_limit,
    );
    let cache = CallGraphCache::new(32);
    cache.put(key.clone(), Arc::new(output));

    let mut group = c.benchmark_group("call_graph_cache");
    group.sample_size(10);

    group.bench_function("cold_miss", |b| {
        b.iter(|| {
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_focused_with_progress_with_entries(
                std::hint::black_box(root),
                std::hint::black_box(&params),
                std::hint::black_box(&progress),
                std::hint::black_box(&ct),
                std::hint::black_box(&entries),
                None,
            )
        });
    });

    group.bench_function("warm_hit", |b| {
        b.iter(|| cache.get(std::hint::black_box(&key)));
    });

    group.finish();
}

fn structural_graph_benchmark(c: &mut Criterion) {
    use aptu_coder_core::cache::StructuralGraphCache;

    let root = Path::new("src");
    let entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();

    let file_outputs: Vec<_> = entries
        .iter()
        .filter(|e| !e.is_dir && e.path.extension().is_some_and(|ext| ext == "rs"))
        .map(|e| aptu_coder_core::analyze::analyze_file(e.path.to_str().unwrap(), None).unwrap())
        .collect();

    // Compute cache key from content hashes
    let mut hashes = Vec::new();
    for e in &entries {
        if !e.is_dir && !e.is_symlink {
            if let Ok(bytes) = std::fs::read(&e.path) {
                let hash = blake3::hash(&bytes);
                hashes.push((e.path.clone(), hash));
            }
        }
    }
    let cache_key = aptu_coder_core::graph::GraphDiskStore::cache_key(root, &hashes);

    // Pre-build and populate cache
    let graph = std::sync::Arc::new(StructuralGraph::build_from_analysis(&file_outputs));
    let sg_cache = StructuralGraphCache::new(16);
    sg_cache.put(cache_key.clone(), graph.clone());

    let mut group = c.benchmark_group("structural_graph");
    group.sample_size(10);

    group.bench_function("cold_miss", |b| {
        b.iter(|| StructuralGraph::build_from_analysis(std::hint::black_box(&file_outputs)));
    });

    group.bench_function("warm_hit", |b| {
        b.iter(|| sg_cache.get(std::hint::black_box(&cache_key)));
    });

    group.finish();
}

criterion_group!(
    benches,
    overview_benchmark,
    file_details_benchmark,
    symbol_focus_benchmark,
    subtree_count_overhead,
    subtree_count_overhead_500,
    subtree_count_overhead_1000,
    analyze_module_benchmark,
    analyze_directory_depth_benchmark,
    call_graph_cache_benchmark,
    structural_graph_benchmark
);
criterion_main!(benches);
