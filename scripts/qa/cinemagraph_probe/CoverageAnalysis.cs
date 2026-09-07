namespace GifFromScreen.CinemagraphProbe;

internal sealed record PixelDifference(int X, int Y, byte[] Expected, byte[] Actual);
internal sealed record Difference(int DifferentPixels, int DifferentChannels, int MaxChannelError,
    IReadOnlyList<PixelDifference> Witnesses);
internal sealed record Coefficients(int Count, int? Minimum, int? Maximum);
internal sealed record CoverageWitness(int X, int Y, byte WhiteAlpha, byte[] FirstPm, byte[] ClipPm,
    Coefficients Mask8, Coefficients Coverage64);
internal sealed record CoverageReport(Difference Mask8FromWhite, Difference Coverage64FromWhite,
    int WhiteAlphaOutsideCoverage64, int PixelsWithNoMask8Coefficient, int PixelsWithNoCoverage64Coefficient,
    IReadOnlyList<CoverageWitness> Witnesses);
internal sealed record CoverageResult(CoverageReport Report, byte[] Alpha8, byte[] Count64,
    byte[] Mask8Prediction, byte[] Coverage64Prediction);

internal static class CoverageAnalysis
{
    // Diagnostic hypotheses only. Neither function manufactures the actual WPF outputs.
    private static byte Mask8(byte channel, int alpha)
    {
        var value = channel * alpha + 128;
        return (byte)((value + (value >> 8)) >> 8);
    }
    // dotnet/wpf a04736ac core/sw/swlib/aarasterizer.cpp: scale=count*4, divide by 256.
    private static byte Coverage64(byte channel, int count) => (byte)((channel * count * 4 + 128) >> 8);

    internal static CoverageResult Analyze(byte[] firstPm, byte[] whitePm, byte[] clippedPm, int width)
    {
        RequireSame(firstPm, whitePm);
        RequireSame(firstPm, clippedPm);
        var pixels = firstPm.Length / 4;
        var alpha8 = new byte[pixels];
        var count64 = new byte[pixels];
        var mask8Prediction = new byte[firstPm.Length];
        var coverage64Prediction = new byte[firstPm.Length];
        var unsupportedWhite = 0;
        var no8 = 0;
        var no64 = 0;
        var witnesses = new List<CoverageWitness>();
        for (var pixel = 0; pixel < pixels; pixel++)
        {
            var offset = pixel * 4;
            var alpha = whitePm[offset + 3];
            alpha8[pixel] = alpha;
            var inferred64 = Enumerable.Range(0, 65).Where(count => Coverage64(255, count) == alpha).ToArray();
            count64[pixel] = inferred64.Length == 1 ? (byte)inferred64[0] : (byte)255;
            if (inferred64.Length != 1) unsupportedWhite++;
            for (var channel = 0; channel < 4; channel++)
            {
                mask8Prediction[offset + channel] = Mask8(firstPm[offset + channel], alpha);
                if (inferred64.Length == 1)
                    coverage64Prediction[offset + channel] = Coverage64(firstPm[offset + channel], inferred64[0]);
            }
            var matching8 = Matching(firstPm, clippedPm, offset, 256, Mask8);
            var matching64 = Matching(firstPm, clippedPm, offset, 65, Coverage64);
            if (matching8.Count == 0) no8++;
            if (matching64.Count == 0) no64++;
            if (witnesses.Count < 16 && (matching8.Count == 0 || matching64.Count == 0 ||
                !firstPm.AsSpan(offset, 4).SequenceEqual(clippedPm.AsSpan(offset, 4)) &&
                (!mask8Prediction.AsSpan(offset, 4).SequenceEqual(clippedPm.AsSpan(offset, 4)) || inferred64.Length != 1 ||
                 !coverage64Prediction.AsSpan(offset, 4).SequenceEqual(clippedPm.AsSpan(offset, 4)))))
                witnesses.Add(new CoverageWitness(pixel % width, pixel / width, alpha,
                    firstPm[offset..(offset + 4)], clippedPm[offset..(offset + 4)], matching8, matching64));
        }
        return new CoverageResult(new CoverageReport(Compare(mask8Prediction, clippedPm, width),
            Compare(coverage64Prediction, clippedPm, width), unsupportedWhite, no8, no64, witnesses),
            alpha8, count64, mask8Prediction, coverage64Prediction);
    }

    private static Coefficients Matching(byte[] source, byte[] actual, int offset, int count,
        Func<byte, int, byte> multiply)
    {
        var matches = 0;
        int? minimum = null;
        int? maximum = null;
        for (var coefficient = 0; coefficient < count; coefficient++)
        {
            var equal = true;
            for (var channel = 0; channel < 4; channel++)
                equal &= multiply(source[offset + channel], coefficient) == actual[offset + channel];
            if (!equal) continue;
            matches++;
            minimum ??= coefficient;
            maximum = coefficient;
        }
        return new Coefficients(matches, minimum, maximum);
    }

    internal static byte[] SourceOver(byte[] sourcePm, byte[] currentPm)
    {
        RequireSame(sourcePm, currentPm);
        var result = new byte[sourcePm.Length];
        for (var offset = 0; offset < result.Length; offset += 4)
        for (var channel = 0; channel < 4; channel++)
            result[offset + channel] = (byte)Math.Min(255,
                sourcePm[offset + channel] + Mask8(currentPm[offset + channel], 255 - sourcePm[offset + 3]));
        return result;
    }

    internal static Difference Compare(byte[] expected, byte[] actual, int width)
    {
        RequireSame(expected, actual);
        var pixels = 0;
        var channels = 0;
        var maximum = 0;
        var witnesses = new List<PixelDifference>();
        for (var offset = 0; offset < expected.Length; offset += 4)
        {
            var changed = false;
            for (var channel = 0; channel < 4; channel++)
            {
                var difference = Math.Abs(expected[offset + channel] - actual[offset + channel]);
                if (difference == 0) continue;
                changed = true;
                channels++;
                maximum = Math.Max(maximum, difference);
            }
            if (!changed) continue;
            pixels++;
            if (witnesses.Count < 16) witnesses.Add(new PixelDifference(offset / 4 % width, offset / 4 / width,
                expected[offset..(offset + 4)], actual[offset..(offset + 4)]));
        }
        return new Difference(pixels, channels, maximum, witnesses);
    }

    private static void RequireSame(byte[] first, byte[] second)
    {
        if (first.Length != second.Length || first.Length % 4 != 0)
            throw new InvalidDataException("Pixel comparisons require equally sized packed four-channel images.");
    }
}
