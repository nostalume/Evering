use std::{
    fs,
    path::{Path, PathBuf},
};

use plotters::prelude::*;

use super::{
    analysis::{self, Analysis, Authority},
    model::Study,
};

const SIZE: (u32, u32) = (1500, 760);
const BLUE: RGBColor = RGBColor(37, 99, 235);
const INK: RGBColor = RGBColor(51, 65, 85);
const GRID: RGBColor = RGBColor(203, 213, 225);

fn size(value: u64) -> String {
    if value == 0 {
        "empty".into()
    } else if value.is_multiple_of(1 << 20) {
        format!("{} MiB", value >> 20)
    } else if value.is_multiple_of(1 << 10) {
        format!("{} KiB", value >> 10)
    } else {
        format!("{value} B")
    }
}

fn label(analysis: &Analysis, index: usize) -> String {
    let estimate = &analysis.estimates[index];
    let condition = &estimate.condition;
    let arm = analysis
        .specification
        .arm(estimate.candidate)
        .expect("admitted arm");
    let mut text = format!(
        "{} · {} payload · queue {} · {} in flight · shared memory {:.3} MiB",
        arm.label,
        size(condition.payload),
        condition.capacity,
        condition.in_flight,
        condition.memory as f64 / (1 << 20) as f64,
    );
    if let Authority::Focused(decisions) = &analysis.authority {
        text += &format!(" · {:?}", decisions[index]);
    }
    text
}

fn draw(analysis: &Analysis) -> Result<String, String> {
    let mut svg = String::new();
    {
        let root = SVGBackend::with_string(&mut svg, SIZE).into_drawing_area();
        root.fill(&WHITE).map_err(|error| error.to_string())?;
        let family = analysis.specification;
        let descriptive = matches!(analysis.authority, Authority::Screening);
        let authority = if descriptive {
            "screening descriptive only"
        } else {
            "focused registered decisions"
        };
        root.draw(&Text::new(
            format!("Throughput ratios — {}/v{}", family.key, family.revision),
            (40, 35),
            ("sans-serif", 25).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.draw(&Text::new(
            format!(
                "Baseline: {} · {} paired blocks · {}",
                family.baseline.label, analysis.meta.blocks, authority
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
                    label(analysis, count - 1 - *row)
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
                "Evidence {}:{} · intervals are conditioned estimates; families are not pooled or ranked",
                analysis.meta.revision, analysis.meta.schedule
            ),
            (40, 735),
            ("sans-serif", 13).into_font().color(&INK),
        ))
        .map_err(|error| error.to_string())?;
        root.present().map_err(|error| error.to_string())?;
    }
    Ok(svg)
}

pub fn render(output: &Path, studies: &[Study]) -> Result<Vec<PathBuf>, String> {
    let analyses = analysis::analyses(studies).map_err(|error| format!("{error:?}"))?;
    let rendered = analyses
        .iter()
        .map(|analysis| Ok((analysis.specification.key, draw(analysis)?)))
        .collect::<Result<Vec<_>, String>>()?;
    fs::create_dir(output).map_err(|error| format!("{}: {error}", output.display()))?;
    rendered
        .into_iter()
        .map(|(family, svg)| {
            let path = output.join(format!("{family}.svg"));
            fs::write(&path, svg).map_err(|error| format!("{}: {error}", path.display()))?;
            Ok(path)
        })
        .collect()
}

pub fn command(output: &Path, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let studies = paths
        .iter()
        .map(|path| analysis::complete(path))
        .collect::<Result<Vec<_>, _>>()?;
    render(output, &studies)
}
