import pathlib, re, subprocess
p = '.github/pull_request_template.md'
old = subprocess.check_output(['git', 'show', 'HEAD:' + p], text=True)
new = pathlib.Path(p).read_text()
strip = lambda s: re.sub(r'<!--.*?-->', '', s, flags=re.S)
assert strip(old) == strip(new), 'noncomment bytes changed'
assert re.findall(r'^## .+$', old, re.M) == re.findall(r'^## .+$', new, re.M)
assert new.count('Describe the concrete user, product, or operational problem.') == 1
maintainer = '''**MUST:** Keep **Allow edits from maintainers** enabled for this PR so maintainers
can help update the branch when needed.'''
assert maintainer in old and maintainer in new
assert old.split('## User Impact')[1].split('## Evidence')[0] == new.split('## User Impact')[1].split('## Evidence')[0], 'impact/rationale guidance changed'
assert re.findall(r'^- \[[ xX]\].*$',old,re.M) == re.findall(r'^- \[[ xX]\].*$',new,re.M)
changed = subprocess.check_output(['git','diff','--name-only','HEAD'],text=True).splitlines()
assert changed == [p], changed
print('PASS: only template HTML comments changed; all noncomment bytes, four headings, maintainer-edit requirement, checklists, impact/rationale and literal problem prompt preserved.')
print('Candidate template blob:', subprocess.check_output(['git','hash-object',p],text=True).strip())
