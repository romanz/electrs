---
name: Bug report
about: Generic bug report
title: 'Bug:'
labels: bug
assignees: ''

---

<!--
	If you use electrs integrated into other project report the bug to their project
	(unless you are the project author who found the bug is in electrs itself)!
	If electrs is crashing due to low memory try with jsonrpc-import first!
-->

**Describe the bug**
A clear and concise description of what the bug is.

**To Reproduce**
Steps to reproduce the behavior:
1. Configure and start electrs
2. Connect with electrum client XYZ
3. Wait
4. See error

**Expected behavior**
A clear and concise description of what you expected to happen.

**Configuration**
<!-- repeat the whole details block if you use multiple config files -->

<details>
<summary>electrs.toml</summary>

```
type error message here
```

</details>

Environment variables: `ELECTRS_X=Y;...`
Arguments: `--foo`

**System running electrs**
 - Deployment method: manual/native OS package/Docker
 - OS name and version (name of distribution and version in case of Linux)

**Electrum client**
Client name (if not upstream desktop Electrum) and version:

**Additional context**
Add any other context about the problem here.
