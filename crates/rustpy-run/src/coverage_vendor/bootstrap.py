"""Launch pinned, unmodified coverage.py and TOML support without installation."""
import os
import pathlib
import runpy
import shutil
import sys
import tarfile
import tempfile

archive = pathlib.Path(sys.argv.pop(1))
package_root = archive.parent / 'coverage-7.6.1'
if not (package_root / '.complete').is_file():
    staging = pathlib.Path(tempfile.mkdtemp(prefix='.coverage-extract-', dir=str(archive.parent)))
    try:
        for package, prefix, strip in [
            (archive, ('coverage-7.6.1', 'coverage'), 1),
            (archive.with_name('tomli-2.0.1.tar.gz'), ('tomli-2.0.1', 'src', 'tomli'), 2),
        ]:
            with tarfile.open(package, 'r:gz') as distribution:
                for member in distribution:
                    pieces = pathlib.PurePosixPath(member.name).parts
                    if len(pieces) <= len(prefix) or pieces[:len(prefix)] != prefix:
                        continue
                    if not member.isfile() or '..' in pieces:
                        continue
                    target = staging.joinpath(*pieces[strip:])
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with distribution.extractfile(member) as source, target.open('wb') as destination:
                        shutil.copyfileobj(source, destination)
        (staging / '.complete').touch()
        try:
            staging.rename(package_root)
        except OSError:
            if not (package_root / '.complete').is_file():
                raise
    finally:
        if staging.exists():
            shutil.rmtree(staging)
sys.path.insert(0, str(package_root))
os.environ.setdefault('COVERAGE_CORE', 'pytrace')
sys.argv[0] = 'coverage'
runpy.run_module('coverage', run_name='__main__')
