# Reference: the plan in `planning/`

1. One file for each feature: `planning/features/<id>.jsonc`. Run, build and deploy units are in
   `planning/units.jsonc`. The schemas are `planning/feature.schema.json` and `planning/units.schema.json`.
2. A feature has `id` (the file name), `title`, `status` (`planned`, `in-progress`, `done`, `abandoned`),
   `plan_rows` (`<dossier>#<row>`), `objectives`, `stories` and `work_items`.
3. A story (`kind`: story, lifecycle, data-flow, control-flow, requirement, issue) names its objectives and its tests
   (`test:<file>::<fn>`). A work item gets its tests from its stories.
4. A work item has `id`, `text`, `status`, `stories`, `depends_on` and `units`. Each unit has `key`, `change` (`add`,
   `edit`, `remove`) and an optional `loc` estimate.
5. Unit keys: `file:<path>`, `rust:<crate dir>:<module path>::<name>`, `symbol:<file>::impl <type>.<method>`,
   `config:<dotted key>` (in `config/defaults.jsonc`), `op:<operation>` (in `docs/api/schema.json`),
   `test:<file>::<fn>`, `unit:<name>`. `legionctl code graph query <name>` shows the key of a symbol.
6. `code plan check` refuses: a file that does not parse or has an unknown field, a duplicate id, a link to an unknown
   id, an unknown key prefix, an `edit` unit that is not there, a unit or test of a done item or feature that is not
   where its change puts it, and a work item with no tests.
7. `code plan status` shows each unit `done`, `missing` or `still there`, LOC planned and actual (tokei for files and
   units, the span for symbols), the linked tests, and with `--since <ref>` the changes that touch each work item.
8. `dossier check` reports a plan row that says done or built while its feature has open work items, and a feature
   that names a plan row that does not exist.
