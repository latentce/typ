# Corpus source

`1grams_english.csv` is the 10,000-word English unigram list from
[orgtre/google-books-ngram-frequency](https://github.com/orgtre/google-books-ngram-frequency)
(`ngrams/1grams_english.csv`, retrieved 2026-09-22). Each row is
`ngram,freq,cumshare`: the word, its raw occurrence count in the Google Books
Ngram Viewer Exports (version 3, English, 2010–2019), and its cumulative share
of tokens. The upstream repository hand-cleans the list of person, place, and
company names, abbreviations, and word fragments.

Both the list and the underlying Google Books Ngram data are licensed under the
[Creative Commons Attribution 3.0 Unported License](https://creativecommons.org/licenses/by/3.0/);
the full text is in `LICENSE` in this directory. The file is unmodified.

`typ` compiles this file into `typ-rs-core` and derives the corpus from it at
startup: lowercase `a–z` only, single letters other than `a` and `i` dropped,
top 10,000 kept. The source has exactly 10,000 rows, 180 of them capitalised
(`I`, `God`, month names), so the corpus holds 9,820 words. Changing this file
is a corpus version bump.
