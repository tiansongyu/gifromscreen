"""Extract the regular-file-only portable format into a caller-owned directory."""

from pathlib import PurePosixPath
import shutil
import tarfile


def extract_checked(archive, destination):
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        seen = set()
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or member.name in seen or not (member.isdir() or member.isfile()):
                raise ValueError("unsafe/duplicate archive member: " + member.name)
            seen.add(member.name)
        for member in members:
            path = destination / member.name
            if member.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as data, path.open("xb") as output:
                    shutil.copyfileobj(data, output)
                path.chmod(member.mode)
    roots = list(destination.iterdir())
    if len(roots) != 1:
        raise ValueError("archive must have one containing directory")
    return roots[0]
