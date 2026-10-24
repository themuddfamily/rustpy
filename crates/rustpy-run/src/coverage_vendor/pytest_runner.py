# Runs inside the measured pytest process after optional bundled initialization.
import json
import coverage
options = json.loads(sys.argv.pop(1))
_original_coverage = coverage.Coverage
class ConfiguredCoverage(_original_coverage):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        if options['include']:
            self.config.include = options['include']
            self.config.source = None
            self.config.source_pkgs = []
        self.config.omit = list(self.config.omit or []) + options['omit']
coverage.Coverage = ConfiguredCoverage
sys.argv[0] = 'pytest'
runpy.run_module('pytest', run_name='__main__')
