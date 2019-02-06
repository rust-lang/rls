#!/bin/bash
# This script reproduces all save-analysis data in the test_data directories.

# $1 is where to run cargo build
# $2 is the output dir
function build {
    echo "$1 => $2"
    output="$(pwd)/$2"
    pushd "$1" > /dev/null
    RUSTFLAGS=-Zsave-analysis cargo build
    cp target/debug/deps/save-analysis/*.json "$output"
    # strip all hashes from filenames libfoo-[hash].json -> libfoo.json
    for from in $output/*.json; do
        to=$(echo "$from" | sed -e "s/\(.*\)-[a-f0-9]*.json/\1.json/1")
        mv "$from" "$to"
    done
    popd > /dev/null
}

# Data for rls-analysis. This is essentially a bootstrap. Be careful when using
# this data because the source is not pinned, therefore the data will change
# regularly. It should basically just be used as a 'big'-ish set of real-world
# data for smoke testing.

rm rls-analysis/*.json
build .. rls-analysis

# Hello world test case
build hello hello/save-analysis

# Types
build types types/save-analysis

# Expressions
build exprs exprs/save-analysis

# all_ref_unique
build rename rename/save-analysis
