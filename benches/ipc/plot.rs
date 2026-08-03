use std::{
    fs,
    path::{Path, PathBuf},
};

use plotters::prelude::*;

use super::analysis::{self, Analysis, MechanismAnalysis, Report};

const SIZE: (u32, u32) = (1500, 760);
const RETENTION_LIMIT: u64 = 5 * 1024 * 1024;
const BLUE: RGBColor = RGBColor(37, 99, 235);
const AMBER: RGBColor = RGBColor(217, 119, 6);
const INK: RGBColor = RGBColor(51, 65, 85);
const GRID: RGBColor = RGBColor(203, 213, 225);

fn draw_system(analysis: &Analysis) -> Result<String, String> {
    let mut svg = String::new();
    {
        let root = SVGBackend::with_string(&mut svg, SIZE).into_drawing_area();
        root.fill(&WHITE).map_err(|error| error.to_string())?;
        let family = analysis.family;
        let authority = format!("{:?} authority", analysis.authority);
        root.draw(&Text::new(
            format!("Throughput ratios — {}/v{}", family.key, family.revision),
            (40, 35),
            ("sans-serif", 25).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.draw(&Text::new(
            format!(
                "Baseline: {} · {} paired blocks · {}",
                family.baseline.label, analysis.specification.blocks, authority
            ),
            (40, 68),
            ("sans-serif", 16).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;

        let count = analysis.estimates.len();
        let minimum = analysis
            .estimates
            .iter()
            .map(|estimate| estimate.low)
            .fold(0.8_f64, f64::min)
            * 0.9;
        let maximum = analysis
            .estimates
            .iter()
            .map(|estimate| estimate.high)
            .fold(1.2_f64, f64::max)
            * 1.1;
        let mut chart = ChartBuilder::on(&root)
            .margin_top(95)
            .margin_right(55)
            .margin_bottom(65)
            .margin_left(10)
            .set_label_area_size(LabelAreaPosition::Left, 500)
            .set_label_area_size(LabelAreaPosition::Bottom, 45)
            .build_cartesian_2d((minimum..maximum).log_scale(), 0..count)
            .map_err(|error| error.to_string())?;
        chart
            .configure_mesh()
            .disable_y_mesh()
            .x_desc("throughput ratio (candidate / baseline, log scale)")
            .x_label_formatter(&|value| format!("{value:.2}×"))
            .y_labels(count)
            .y_label_formatter(&|row| {
                if *row < count {
                    analysis::system_label(analysis, count - 1 - *row)
                } else {
                    String::new()
                }
            })
            .axis_style(INK)
            .light_line_style(GRID.mix(0.45))
            .label_style(("sans-serif", 14).into_font().color(&INK))
            .draw()
            .map_err(|error| error.to_string())?;
        chart
            .draw_series([PathElement::new(
                vec![(1.0, 0), (1.0, count.saturating_sub(1))],
                GRID.stroke_width(2),
            )])
            .map_err(|error| error.to_string())?;
        for (index, estimate) in analysis.estimates.iter().enumerate() {
            let row = count - 1 - index;
            chart
                .draw_series([
                    PathElement::new(
                        vec![(1.0 - estimate.delta, row), (1.0 + estimate.delta, row)],
                        BLUE.mix(0.16).stroke_width(9),
                    )
                    .into_dyn(),
                    PathElement::new(
                        vec![(estimate.low, row), (estimate.high, row)],
                        INK.stroke_width(2),
                    )
                    .into_dyn(),
                    Circle::new((estimate.effect, row), 5, BLUE.filled()).into_dyn(),
                ])
                .map_err(|error| error.to_string())?;
        }
        root.draw(&Text::new(
            format!(
                "Evidence {}:{} · translucent line is the preregistered practical band · families are not pooled or ranked",
                analysis.context.source.revision, analysis.evidence
            ),
            (40, 735),
            ("sans-serif", 13).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.present().map_err(|error| error.to_string())?;
    }
    Ok(svg)
}

fn draw_mechanism(
    analysis: &MechanismAnalysis,
    authority: analysis::Authority,
) -> Result<String, String> {
    let mut svg = String::new();
    {
        let root = SVGBackend::with_string(&mut svg, SIZE).into_drawing_area();
        root.fill(&WHITE).map_err(|error| error.to_string())?;
        root.draw(&Text::new(
            format!("Mechanism paired differences — {}", analysis.schema),
            (40, 35),
            ("sans-serif", 25).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.draw(&Text::new(
            format!(
                "Gross minus control · {:?} authority · practical band ±{:.3} ns/op · expected System change {:+.3}",
                authority, analysis.delta_ns, analysis.system_delta
            ),
            (40, 68),
            ("sans-serif", 16).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        let count = analysis.estimates.len();
        let (mut minimum, mut maximum) = analysis
            .estimates
            .iter()
            .flat_map(|estimate| estimate.paired_ns.iter().copied())
            .chain([-analysis.delta_ns, 0.0, analysis.delta_ns])
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
                (low.min(value), high.max(value))
            });
        let margin = ((maximum - minimum) * 0.1).max(0.5);
        minimum -= margin;
        maximum += margin;
        let mut chart = ChartBuilder::on(&root)
            .margin_top(95)
            .margin_right(55)
            .margin_bottom(65)
            .margin_left(10)
            .set_label_area_size(LabelAreaPosition::Left, 500)
            .set_label_area_size(LabelAreaPosition::Bottom, 45)
            .build_cartesian_2d(minimum..maximum, 0..count)
            .map_err(|error| error.to_string())?;
        chart
            .configure_mesh()
            .disable_y_mesh()
            .x_desc("paired difference (gross − control, ns/op)")
            .x_label_formatter(&|value| format!("{value:.2}"))
            .y_labels(count)
            .y_label_formatter(&|row| {
                if *row < count {
                    analysis.estimates[count - 1 - *row].case.clone()
                } else {
                    String::new()
                }
            })
            .axis_style(INK)
            .light_line_style(GRID.mix(0.45))
            .label_style(("sans-serif", 14).into_font().color(&INK))
            .draw()
            .map_err(|error| error.to_string())?;
        chart
            .draw_series([PathElement::new(
                vec![(0.0, 0), (0.0, count.saturating_sub(1))],
                GRID.stroke_width(2),
            )])
            .map_err(|error| error.to_string())?;
        for (index, estimate) in analysis.estimates.iter().enumerate() {
            let row = count - 1 - index;
            chart
                .draw_series(
                    estimate
                        .paired_ns
                        .iter()
                        .map(|value| Circle::new((*value, row), 3, BLUE.mix(0.35).filled())),
                )
                .map_err(|error| error.to_string())?;
            chart
                .draw_series([
                    PathElement::new(
                        vec![(estimate.iqr[0], row), (estimate.iqr[1], row)],
                        INK.stroke_width(3),
                    )
                    .into_dyn(),
                    Circle::new((estimate.interval.point, row), 6, AMBER.filled()).into_dyn(),
                ])
                .map_err(|error| error.to_string())?;
        }
        root.draw(&Text::new(
            format!(
                "Evidence {} · points are paired batches; bar is interquartile range; amber point is median",
                analysis.evidence
            ),
            (40, 735),
            ("sans-serif", 13).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.present().map_err(|error| error.to_string())?;
    }
    Ok(svg)
}

fn name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

pub fn render(output: &Path, report: &Report) -> Result<Vec<PathBuf>, String> {
    let mut rendered = vec![("report.md".into(), report.markdown())];
    for analysis in &report.systems {
        rendered.push((
            format!(
                "system-{}-v{}-{}-{}.svg",
                name(analysis.family.key),
                analysis.family.revision,
                name(&analysis.specification.mode),
                analysis.evidence
            ),
            draw_system(analysis)?,
        ));
    }
    for section in &report.mechanisms {
        let authority = section
            .attribution
            .as_ref()
            .map_or(analysis::Authority::Descriptive, |link| link.authority);
        rendered.push((
            format!(
                "mechanism-{}-{}.svg",
                name(&section.analysis.schema),
                section.analysis.evidence
            ),
            draw_mechanism(&section.analysis, authority)?,
        ));
    }
    rendered.sort_by(|left, right| left.0.cmp(&right.0));
    let mut manifest = String::new();
    use core::fmt::Write;
    for source in &report.sources {
        writeln!(manifest, "{}  source:{}", source.digest, source.evidence)
            .expect("String writes cannot fail");
    }
    for (name, bytes) in &rendered {
        writeln!(
            manifest,
            "{}  {name}",
            blake3::hash(bytes.as_bytes()).to_hex()
        )
        .expect("String writes cannot fail");
    }
    fs::create_dir(output).map_err(|error| format!("{}: {error}", output.display()))?;
    let mut paths = Vec::with_capacity(rendered.len() + 1);
    for (name, bytes) in rendered {
        let path = output.join(name);
        write_new(&path, bytes.as_bytes())?;
        paths.push(path);
    }
    let manifest_path = output.join("DIGESTS.blake3");
    write_new(&manifest_path, manifest.as_bytes())?;
    paths.push(manifest_path);
    Ok(paths)
}

pub fn command(output: &Path, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    for path in paths {
        let bytes = fs::metadata(path)
            .map_err(|error| format!("{path}: {error}"))?
            .len();
        if bytes > RETENTION_LIMIT {
            return Err(format!(
                "{path}: {bytes} bytes exceeds the {RETENTION_LIMIT}-byte retention limit"
            ));
        }
    }
    render(output, &analysis::report(paths)?)
}
