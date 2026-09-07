using System.Security.Cryptography;

namespace GifFromScreen.CinemagraphProbe;

internal sealed record Artifact(string File, string Format, int Bytes, string Sha256);

internal sealed class ArtifactWriter
{
    private readonly string root;
    internal long TotalBytes { get; private set; }
    internal ArtifactWriter(string requested)
    {
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(requested));
        var parent = Directory.GetParent(root);
        if (parent is null || !parent.Exists || root.StartsWith(@"\\", StringComparison.Ordinal))
            throw new InvalidDataException("Output must be a new local directory in an existing parent.");
        if (Exists(root)) throw new IOException("Output already exists; nothing will be overwritten.");
        Directory.CreateDirectory(root);
    }

    internal Artifact Write(string relative, string format, byte[] bytes)
    {
        Limits.Check("write artifact");
        if (Path.IsPathRooted(relative) || relative.Contains('\\') || relative.Split('/').Any(p => p is "" or "." or ".."))
            throw new InvalidDataException("Artifact path must be a safe relative path.");
        var next = checked(TotalBytes + bytes.LongLength);
        if (next > Limits.MaxOutputBytes) throw new InvalidDataException("Probe output exceeds 16 MiB.");
        var path = Path.GetFullPath(Path.Combine(root, relative.Replace('/', Path.DirectorySeparatorChar)));
        if (!path.StartsWith(root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Artifact escaped its owned output directory.");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        using var stream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
        stream.Write(bytes);
        stream.Flush(flushToDisk: true);
        TotalBytes = next;
        return new Artifact(relative, format, bytes.Length, Hash(bytes));
    }

    internal static string Hash(byte[] bytes) => Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
    private static bool Exists(string path)
    {
        try { _ = File.GetAttributes(path); return true; }
        catch (FileNotFoundException) { return false; }
        catch (DirectoryNotFoundException) { return false; }
    }
}
