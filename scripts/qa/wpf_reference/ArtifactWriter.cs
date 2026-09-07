using System.Security.Cryptography;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace GifFromScreen.WpfReference;

internal sealed record BitmapArtifact(int Width, int Height, string RgbaFile, string RgbaSha256,
    string PngFile, string PngSha256, string PremultipliedFile, string PremultipliedSha256);
internal sealed record BitmapSnapshot(BitmapSource Decoded, BitmapArtifact Artifact);

internal sealed class ArtifactWriter
{
    private readonly string root;
    internal long TotalBytes { get; private set; }

    internal ArtifactWriter(string requested)
    {
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(requested));
        var parent = Directory.GetParent(root);
        if (parent is null || !parent.Exists || root.StartsWith(@"\\", StringComparison.Ordinal))
            throw new InvalidDataException("Output must be a new local directory inside an existing parent.");
        if (PathExists(root)) throw new IOException("Output directory already exists; it will not be reused or overwritten.");
        // The CI wrapper supplies an owned, unique runner-temp path. Every file
        // below is also CreateNew; failures never overwrite earlier artifacts.
        Directory.CreateDirectory(root);
    }

    internal BitmapSnapshot Snapshot(string id, string name, BitmapSource rendered)
    {
        Limits.Size(rendered.PixelWidth, rendered.PixelHeight);
        Limits.Check($"{id}/{name}: snapshot");
        var png = Encode(rendered);
        var decoded = Decode(png);
        if (decoded.PixelWidth != rendered.PixelWidth || decoded.PixelHeight != rendered.PixelHeight)
            throw new InvalidDataException("WIC PNG round trip changed the bitmap dimensions.");
        var rgba = ReadPixels(decoded, PixelFormats.Bgra32);
        for (var index = 0; index < rgba.Length; index += 4)
            (rgba[index], rgba[index + 2]) = (rgba[index + 2], rgba[index]);
        // These are the real pre-PNG RTB bytes (or converted input bytes), not
        // a hand-coded premultiplication of the already decoded straight RGBA.
        var premultiplied = ReadPixels(rendered, PixelFormats.Pbgra32);
        var stem = $"{id}/{name}";
        Write($"{stem}.png", png);
        Write($"{stem}.rgba", rgba);
        Write($"{stem}.pbgra", premultiplied);
        var artifact = new BitmapArtifact(decoded.PixelWidth, decoded.PixelHeight,
            $"{stem}.rgba", Hashing.Bytes(rgba), $"{stem}.png", Hashing.Bytes(png),
            $"{stem}.pbgra", Hashing.Bytes(premultiplied));
        return new BitmapSnapshot(decoded, artifact);
    }

    internal void Write(string relative, byte[] bytes)
    {
        if (Path.IsPathRooted(relative) || relative.Contains('\\') || relative.Split('/').Any(part => part is "" or "." or ".."))
            throw new InvalidDataException("Artifact path must be a safe POSIX relative path.");
        var next = checked(TotalBytes + bytes.LongLength);
        if (next > Limits.MaxOutputBytes) throw new InvalidDataException("Reference artifacts exceed 16 MiB.");
        var path = Path.GetFullPath(Path.Combine(root, relative.Replace('/', Path.DirectorySeparatorChar)));
        if (!path.StartsWith(root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Artifact escaped its new output directory.");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
        stream.Write(bytes);
        stream.Flush(flushToDisk: true);
        TotalBytes = next;
    }

    private static bool PathExists(string path)
    {
        try { _ = File.GetAttributes(path); return true; }
        catch (FileNotFoundException) { return false; }
        catch (DirectoryNotFoundException) { return false; }
    }

    private static byte[] Encode(BitmapSource bitmap)
    {
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var memory = new MemoryStream();
        encoder.Save(memory);
        if (memory.Length > Limits.MaxOutputBytes) throw new InvalidDataException("Encoded PNG exceeds the artifact budget.");
        return memory.ToArray();
    }

    private static BitmapSource Decode(byte[] png)
    {
        using var memory = new MemoryStream(png, writable: false);
        var decoder = BitmapDecoder.Create(memory, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad);
        if (decoder.Frames.Count != 1) throw new InvalidDataException("Expected one PNG image.");
        var image = decoder.Frames[0];
        image.Freeze();
        return image;
    }

    private static byte[] ReadPixels(BitmapSource bitmap, PixelFormat format)
    {
        BitmapSource converted = bitmap.Format == format ? bitmap : new FormatConvertedBitmap(bitmap, format, null, 0);
        var stride = checked(bitmap.PixelWidth * 4);
        var bytes = new byte[checked(stride * bitmap.PixelHeight)];
        converted.CopyPixels(bytes, stride, 0);
        return bytes;
    }
}

internal static class Hashing
{
    internal static string Bytes(byte[] bytes) => Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();

    internal static string File(string path, long maximumBytes)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        if (stream.Length > maximumBytes) throw new InvalidDataException($"Provenance file exceeds its size bound: {Path.GetFileName(path)}");
        return Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
    }
}
