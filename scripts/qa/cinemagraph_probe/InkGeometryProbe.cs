using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Ink;
using System.Windows.Input;
using System.Windows.Media;

namespace GifFromScreen.CinemagraphProbe;

internal sealed record InkProbeMatrix(double M11, double M12, double M21, double M22, double OffsetX, double OffsetY);
internal sealed record InkProbeAttributes(double Width, double Height, string Tip, bool FitToCurve,
    bool IgnorePressure, bool IsHighlighter, InkProbeMatrix StylusTipTransform);
internal sealed record InkProbeBounds(bool Empty, double X, double Y, double Width, double Height);
internal sealed record InkProbeGeometry(string Type, string Path, string FillRule,
    InkProbeBounds Bounds, InkProbeBounds StrokeBounds, int Figures, int Segments, int ControlPoints);
internal sealed record InkProbeStroke(InkProbeAttributes Attributes, IReadOnlyList<PointSpec> RawSamples,
    IReadOnlyList<PointSpec> GetBezierStylusPoints, IReadOnlyList<PointSpec> EffectiveSamples, InkProbeGeometry Geometry);

/// <summary>
/// Independent managed-WPF diagnostics. This does not change the 90 image cases,
/// does not manufacture reference results with Rust, and does not rasterize ink.
/// </summary>
internal static class InkGeometryProbe
{
    private const int MaximumJsonBytes = 2 * 1024 * 1024;
    private const int MaximumRawSamples = 64;
    private const int MaximumFittedSamples = 4096;
    private const int MaximumPathCharacters = 65536;
    private const double MaximumCoordinate = 64;

    internal static Artifact Write(ArtifactWriter writer)
    {
        var budget = new GeometryBudget();
        var outlines = new List<object>();
        foreach (var (name, samples) in CenterCases())
        foreach (var tip in new[] { StylusTip.Ellipse, StylusTip.Rectangle })
        foreach (var fit in new[] { false, true })
        {
            var stroke = Create(samples, tip, fit, false);
            outlines.Add(new { id = $"{name}-{tip}-{(fit ? "fit" : "raw")}", stroke = Observe(stroke, budget) });
        }
        foreach (var tip in new[] { StylusTip.Ellipse, StylusTip.Rectangle })
        {
            foreach (var pressure in new[] { 0f, .5f, 1f })
            foreach (var ignore in new[] { false, true })
            {
                var stroke = Create(new[] { new PointSpec(8.125, 6.375, pressure) }, tip, false, ignore);
                outlines.Add(new { id = $"single-{tip}-p{pressure.ToString(CultureInfo.InvariantCulture)}-ignore{ignore}", stroke = Observe(stroke, budget) });
            }
            // Use the actual overload whose pressure is not supplied by this probe.
            var implicitPressure = new Stroke(new StylusPointCollection(new[] { new StylusPoint(8.125, 6.375) }), Attributes(tip, false, false));
            outlines.Add(new { id = $"single-{tip}-implicit-pressure", stroke = Observe(implicitPressure, budget) });
        }
        if (outlines.Count > 64) throw new InvalidDataException("Ink outline probe exceeds 64 cases.");

        var transforms = TransformCases(budget);
        var erases = EraseCases(budget);
        Limits.Check("serialize independent ink geometry diagnostics");
        var bytes = JsonSerializer.SerializeToUtf8Bytes(new {
            format_version = 1,
            interpretation = "Actual WPF Stroke.GetBezierStylusPoints/GetGeometry/Transform/GetEraseResult diagnostics only. GetBezierStylusPoints is recorded even when FitToCurve=false; effective_samples selects raw versus fitted exactly as StrokeNodeIterator does. No pixel-parity claim follows from generating this file.",
            source_contract = "dotnet/wpf a04736acb8edb533756131d3d5fc55f15cd03d6a Stroke/Stroke2, StrokeNodeEnumerator, Bezier/CuspData and InkCanvasSelection. The runtime/binary provenance is in index.json; this is a separate diagnostic, not part of the original 90-case definition hash.",
            limits = new { max_raw_samples = MaximumRawSamples, max_fitted_samples = MaximumFittedSamples,
                max_path_characters = MaximumPathCharacters, max_coordinate = MaximumCoordinate, max_json_bytes = MaximumJsonBytes },
            outline_cases = outlines, transform_cases = transforms, erase_cases = erases,
            observed_strokes = budget.Observations, observed_samples_and_controls = budget.Values,
        }, Program.Json);
        if (bytes.Length > MaximumJsonBytes) throw new InvalidDataException("Ink geometry diagnostic exceeds 2 MiB.");
        return writer.Write("ink-geometry.json", "json", bytes);
    }

    private static IReadOnlyList<(string Name, PointSpec[] Samples)> CenterCases() => new (string, PointSpec[])[] {
        ("curve-five", new[] { P(1.25,2,.5f), P(3,8,.5f), P(7,10,.5f), P(11,7,.5f), P(14,2,.5f) }),
        ("cusp-six", new[] { P(1,3,.5f), P(4,3,.5f), P(7,3,.5f), P(4,3,.5f), P(1,3,.5f), P(0,5,.5f) }),
        ("loop-seven", new[] { P(3,3,.5f), P(10,3,.5f), P(12,7,.5f), P(10,11,.5f), P(3,11,.5f), P(1,7,.5f), P(3,3,.5f) }),
        ("duplicate-seven", new[] { P(2,2,.5f), P(2,2,0), P(5,5,.4f), P(5,5,1), P(8,3,.8f), P(11,8,.3f), P(11,8,.9f) }),
        ("variable-pressure-six", new[] { P(1.125,4.375,0), P(2.875,2.625,.125f), P(5.125,7.375,.5f), P(8.875,2.125,.875f), P(12.625,6.375,1), P(15.125,3.125,.25f) }),
        // The three-point branch has its own source-specific control-point formula.
        ("parabola-three", new[] { P(0,0,0), P(1,1,.5f), P(2,0,1) }),
    };

    private static PointSpec P(double x, double y, float pressure) => new(x, y, pressure);

    private static DrawingAttributes Attributes(StylusTip tip, bool fit, bool ignore) => new() {
        Width = 4.25, Height = 3.25, StylusTip = tip, FitToCurve = fit,
        IgnorePressure = ignore, IsHighlighter = false,
    };

    private static Stroke Create(IReadOnlyList<PointSpec> samples, StylusTip tip, bool fit, bool ignore)
    {
        if (samples.Count is < 1 or > MaximumRawSamples) throw new InvalidDataException("Ink raw sample count is invalid.");
        foreach (var sample in samples) ValidatePoint(sample);
        return new Stroke(new StylusPointCollection(samples.Select(p => new StylusPoint(p.X, p.Y, p.Pressure))), Attributes(tip, fit, ignore));
    }

    private static List<object> TransformCases(GeometryBudget budget)
    {
        var results = new List<object>();
        var samples = CenterCases()[4].Samples;
        foreach (var tip in new[] { StylusTip.Ellipse, StylusTip.Rectangle })
        foreach (var fit in new[] { false, true })
        foreach (var transform in new[] { new Matrix(1, 0, 0, 1, 2.5, -1.25), new Matrix(1.25, 0, 0, .75, 1.5, -.5) })
        {
            var stroke = Create(samples, tip, fit, false);
            var before = Observe(stroke, budget);
            // This is the same false argument used by InkCanvasSelection.TransformStrokes.
            // It is not an automated pointer/adorner test or Transform(..., true).
            stroke.Transform(transform, applyToStylusTip: false);
            var after = Observe(stroke, budget);
            var unchanged = before.Attributes == after.Attributes;
            if (!unchanged) throw new InvalidDataException("Transform(false) unexpectedly changed drawing attributes.");
            results.Add(new { id = $"transform-{tip}-{fit}-{results.Count}", matrix = Describe(transform),
                apply_to_stylus_tip = false, before, after, attributes_unchanged = unchanged });
        }
        return results;
    }

    private static List<object> EraseCases(GeometryBudget budget)
    {
        var results = new List<object>();
        var samples = new[] { P(1,6,.25f), P(5,6,.5f), P(9,6,.75f), P(13,6,.5f) };
        foreach (var tip in new[] { StylusTip.Ellipse, StylusTip.Rectangle })
        foreach (var fit in new[] { false, true })
        foreach (var hit in new[] { false, true })
        {
            var stroke = Create(samples, tip, fit, false);
            var before = Observe(stroke, budget);
            var path = hit ? new[] { new Point(7, 0), new Point(7, 12) } : new[] { new Point(28, 18), new Point(28, 22) };
            StylusShape shape = tip == StylusTip.Ellipse ? new EllipseStylusShape(2, 3) : new RectangleStylusShape(2, 3);
            var actualHit = stroke.HitTest(path, shape);
            var erased = stroke.GetEraseResult(path, shape);
            if (erased.Count > 8) throw new InvalidDataException("Ink erase probe returned more than eight fragments.");
            var fragments = erased.Select(fragment => Observe(fragment, budget)).ToArray();
            if (!before.RawSamples.SequenceEqual(Samples(stroke.StylusPoints)))
                throw new InvalidDataException("GetEraseResult mutated the original samples.");
            results.Add(new { id = $"erase-{tip}-{fit}-{(hit ? "crossing" : "miss")}",
                eraser = new { path = path.Select(p => new { x = p.X, y = p.Y }).ToArray(), tip = tip.ToString(), width = 2, height = 3 },
                before, hit_test = actualHit, fragments,
                interpretation = "hit_test records the stroke-erasing hit predicate; fragments are actual GetEraseResult, not sample deletion or a polygon approximation.", });
        }
        return results;
    }

    private static InkProbeStroke Observe(Stroke stroke, GeometryBudget budget)
    {
        Limits.Check("observe actual WPF ink geometry");
        var attributes = Describe(stroke.DrawingAttributes);
        var raw = Samples(stroke.StylusPoints);
        if (raw.Count > MaximumRawSamples) throw new InvalidDataException("Ink observed raw sample count exceeds 64.");
        var fitted = Samples(stroke.GetBezierStylusPoints());
        var geometry = stroke.GetGeometry();
        var path = geometry.ToString(CultureInfo.InvariantCulture);
        if (path.Length > MaximumPathCharacters) throw new InvalidDataException("Ink geometry path exceeds 64 KiB characters.");
        var structured = PathGeometry.CreateFromGeometry(geometry);
        if (structured.Figures.Count > 4096) throw new InvalidDataException("Ink geometry exceeds 4096 figures.");
        var segments = 0;
        var controls = 0;
        foreach (var figure in structured.Figures)
        {
            ValidateCoordinate(figure.StartPoint.X); ValidateCoordinate(figure.StartPoint.Y);
            controls = checked(controls + 1);
            segments = checked(segments + figure.Segments.Count);
            foreach (var segment in figure.Segments)
            {
                controls = checked(controls + ValidateSegment(segment));
            }
        }
        if (segments > 32768 || controls > 32768) throw new InvalidDataException("Ink path control count exceeds its bound.");
        if (!raw.SequenceEqual(Samples(stroke.StylusPoints)) || attributes != Describe(stroke.DrawingAttributes))
            throw new InvalidDataException("Geometry observation changed original samples or attributes.");
        budget.Include(raw.Count + fitted.Count + controls);
        return new InkProbeStroke(attributes, raw, fitted, attributes.FitToCurve ? fitted : raw,
            new InkProbeGeometry(geometry.GetType().FullName ?? geometry.GetType().Name, path, structured.FillRule.ToString(),
                Describe(geometry.Bounds), Describe(stroke.GetBounds()), structured.Figures.Count, segments, controls));
    }

    private static IReadOnlyList<PointSpec> Samples(StylusPointCollection points)
    {
        if (points.Count > MaximumFittedSamples) throw new InvalidDataException("Ink fitted sample count exceeds 4096.");
        var result = new PointSpec[points.Count];
        for (var index = 0; index < result.Length; index++)
        {
            var point = points[index];
            var sample = new PointSpec(point.X, point.Y, point.PressureFactor);
            ValidatePoint(sample); result[index] = sample;
        }
        return result;
    }

    private static InkProbeAttributes Describe(DrawingAttributes attributes) => new(attributes.Width, attributes.Height,
        attributes.StylusTip.ToString(), attributes.FitToCurve, attributes.IgnorePressure, attributes.IsHighlighter,
        Describe(attributes.StylusTipTransform));

    private static InkProbeMatrix Describe(Matrix matrix) => new(matrix.M11, matrix.M12, matrix.M21, matrix.M22, matrix.OffsetX, matrix.OffsetY);

    private static int ValidateSegment(PathSegment segment)
    {
        switch (segment)
        {
            case LineSegment line: return ValidatePoints(new[] { line.Point });
            case BezierSegment cubic: return ValidatePoints(new[] { cubic.Point1, cubic.Point2, cubic.Point3 });
            case QuadraticBezierSegment quadratic: return ValidatePoints(new[] { quadratic.Point1, quadratic.Point2 });
            case ArcSegment arc:
                ValidateCoordinate(arc.Size.Width); ValidateCoordinate(arc.Size.Height);
                if (!double.IsFinite(arc.RotationAngle)) throw new InvalidDataException("Non-finite arc rotation.");
                return ValidatePoints(new[] { arc.Point });
            case PolyLineSegment polyLine: return ValidatePoints(polyLine.Points);
            case PolyBezierSegment polyBezier: return ValidatePoints(polyBezier.Points);
            case PolyQuadraticBezierSegment polyQuadratic: return ValidatePoints(polyQuadratic.Points);
            default: throw new InvalidDataException("Unknown WPF path segment type.");
        }
    }

    private static int ValidatePoints(IEnumerable<Point> points)
    {
        var count = 0;
        foreach (var point in points)
        {
            ValidateCoordinate(point.X); ValidateCoordinate(point.Y);
            if (++count > 32768) throw new InvalidDataException("Ink segment control count exceeds its bound.");
        }
        return count;
    }

    private static InkProbeBounds Describe(Rect bounds)
    {
        if (bounds.IsEmpty) return new InkProbeBounds(true, 0, 0, 0, 0);
        ValidateCoordinate(bounds.X); ValidateCoordinate(bounds.Y);
        ValidateCoordinate(bounds.Width); ValidateCoordinate(bounds.Height);
        return new InkProbeBounds(false, bounds.X, bounds.Y, bounds.Width, bounds.Height);
    }

    private static void ValidatePoint(PointSpec point)
    {
        ValidateCoordinate(point.X); ValidateCoordinate(point.Y);
        if (!float.IsFinite(point.Pressure) || point.Pressure is < 0 or > 1)
            throw new InvalidDataException("Ink pressure must be finite in 0..1.");
    }

    private static void ValidateCoordinate(double value)
    {
        if (!double.IsFinite(value) || Math.Abs(value) > MaximumCoordinate)
            throw new InvalidDataException("Ink diagnostic coordinate is outside its finite +/-64 bound.");
    }

    private sealed class GeometryBudget
    {
        internal int Observations { get; private set; }
        internal int Values { get; private set; }
        internal void Include(int count)
        {
            Limits.Check("ink geometry counters");
            Observations = checked(Observations + 1); Values = checked(Values + count);
            if (Observations > 128 || Values > 65536) throw new InvalidDataException("Ink geometry diagnostic exceeds its observation/sample budget.");
        }
    }
}
