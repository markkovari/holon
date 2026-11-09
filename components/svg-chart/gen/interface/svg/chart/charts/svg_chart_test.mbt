test "render bar chart" {
  let slices : Array[@charts.Slice] = [
    { label: "Apples", value: 40.0, color: "" },
    { label: "Bananas", value: 25.0, color: "#ffcc00" },
  ]
  let c : @charts.Chart = {
    kind: @charts.Kind::BAR,
    title: "Fruit Sales",
    data: slices,
    width: 360U,
    height: 220U,
  }
  let svg = @charts.render(c)
  assert_eq(svg.has_prefix("<svg"), true)
  assert_eq(svg.has_suffix("</svg>"), true)
  assert_eq(svg.contains("Fruit Sales"), true)
  assert_eq(svg.contains("Apples"), true)
}

test "render donut and sparkline" {
  let slices : Array[@charts.Slice] = [
    { label: "Direct", value: 60.0, color: "" },
    { label: "Referral", value: 40.0, color: "" },
  ]
  let donut_chart : @charts.Chart = {
    kind: @charts.Kind::DONUT,
    title: "",
    data: slices,
    width: 0U,
    height: 0U,
  }
  let donut_svg = @charts.render(donut_chart)
  assert_eq(donut_svg.has_prefix("<svg"), true)
  assert_eq(donut_svg.has_suffix("</svg>"), true)
  assert_eq(donut_svg.contains("Direct"), true)

  let spark_chart : @charts.Chart = {
    kind: @charts.Kind::SPARKLINE,
    title: "",
    data: slices,
    width: 0U,
    height: 0U,
  }
  let spark_svg = @charts.render(spark_chart)
  assert_eq(spark_svg.has_prefix("<svg"), true)
  assert_eq(spark_svg.has_suffix("</svg>"), true)
}
