import subprocess, sys
ref, out = sys.argv[1], sys.argv[2]
s = subprocess.run(["git", "show", ref + ":scripts/cloud-bench/setup-node.sh"], capture_output=True, check=True).stdout.decode()
a = s.index('  log "Mandrel $MANDREL_VERSION')
b = s.index('native.log', a)
b = s.index('\n', b)
s = s[:a] + s[b + 1:]
s = s.replace(' benchmarks/camel-quarkus/target/*-runner', '')
open(out, 'w', newline='\n').write(s)
