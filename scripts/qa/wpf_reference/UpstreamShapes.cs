namespace GifFromScreen.WpfReference;

internal static class UpstreamShapes
{
    internal const string Commit = "a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd";
    private const string Prefix = "upstream/ScreenToGif/" + Commit + "/ScreenToGif/Controls/Shapes/";
    internal static IReadOnlyList<GeneratorFile> Verify()
    {
        var requested = Environment.GetEnvironmentVariable("GFS_STG_SHAPES_ROOT");
        if (string.IsNullOrWhiteSpace(requested) || !Path.IsPathFullyQualified(requested))
            throw new InvalidDataException("GFS_STG_SHAPES_ROOT must identify the pinned source checkout by absolute path.");
        var root = Path.GetFullPath(requested);
        var result = new List<GeneratorFile>();
        foreach (var (name, expected) in new[]
        {
            ("Triangle.cs", "6c82bdbb0e92d649aa1529155424675ae44a35778204674fdcfb4c52b16fed85"),
            ("Arrow.cs", "b0f910590b942c9f7e2f0619cc50db8e6e7da411ba38a18b5063789b2bc5172b"),
        })
        {
            var path = root;
            RejectLink(path);
            foreach (var component in new[] { "ScreenToGif", "Controls", "Shapes", name })
            {
                path = Path.Combine(path, component);
                RejectLink(path);
            }
            var actual = Hashing.File(path, 256 * 1024);
            if (!string.Equals(actual, expected, StringComparison.Ordinal))
                throw new InvalidDataException($"Pinned ScreenToGif {name} hash mismatch. No reference can be generated.");
            result.Add(new GeneratorFile(Prefix + name, actual));
        }
        return result;
    }

    private static void RejectLink(string path)
    {
        if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
            throw new InvalidDataException("Pinned source paths must not contain reparse points.");
    }
}
