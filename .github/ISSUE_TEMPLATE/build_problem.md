---
name: Build problem
about: Building of electrs failed
title: 'Build:'
labels: bug, build
assignees: Kixunil

---

<!--
        IMPORTANT: IF YOU DON'T FILL THIS TEMPLATE COMPLETELY IT WILL TAKE MORE TIME FOR US TO HELP YOU!
        SOME EXTERNAL ELECTRS GUIDES SUCH AS RASPIBOLT ARE OUTDATED AND DO NOT WORK SO SHOULD NOT BE FOLLOWED!
        Please try with OUR usage instructions first.

       If your build died with SIGKILL, try clearing up some RAM.
       If you have a low-memory device (such as RPi) try cross-compilation first.
-->

**Have you read the documentation?**
Yes. (Please, read usage.md first if you did not.)

**Did you double-check that you installed all dependencies?**
Yes. (Please, double check the dependencies if you didn't.)

**Which command failed?**
`cargo build`

**What was the error message?**

<details>
<summary>Error message</summary>

```
type error message here
```

</details>

**System**
OS name and version: (If Linux, the distribution name and version)
rustc version: (run `rustc --version`)
cargo version: (run `cargo --version`; not guaranteed to be same as rustc version!)

**Compilation**
Linking: static/dynamic
Cross compilation: yes/no
Target architecture: (uname -m on Linux if not cross-compiling)

**Additional context**
Any additional information that seems to be relevant.
