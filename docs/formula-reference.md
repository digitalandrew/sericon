# Formula API reference

The runtime API is embedded in the Sericon executable. Print the reference from the installed version:

```sh
sericon formulas api
```

The [complete text reference](formula-api.txt) covers history, regex matching, byte handling, input ownership, prompt matching, artifacts, file operations and execution limits. AI clients can retrieve the same reference through `sericon_formula_api`.

Start with the [formula guide](formulas.md) for runnable examples and registration. A script can pass syntax validation and still fail at runtime because of an unknown function, missing data or a device-specific prompt; test interactions against the intended target.

<!-- formula-api -->
