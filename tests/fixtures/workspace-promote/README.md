# workspace-promote

Two corpora for the promotion cases (M7-7): `primary` is `main` and holds the
decision a promotion writes; `context` is `context` and holds the archived
decision a promotion names.

The workspace file is written by each case, because the policy
(`allow_cross_corpus_links`) and the archived corpus's `read_only` are the two
facts the cases flip. Every corpus config disables embeddings, so a case reads
no model.
