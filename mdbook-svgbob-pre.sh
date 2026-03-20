#!/usr/bin/env bash
# mdbook preprocessor: replaces ```svgbob code blocks with inline SVG.

if [ "$1" = "supports" ]; then
    [ "$2" = "html" ] && exit 0 || exit 1
fi

python3 -c '
import json, sys, re, subprocess

data = json.load(sys.stdin)

if isinstance(data, list):
    book = data[1]
else:
    book = data

def replace_svgbob(match):
    src = match.group(1)
    result = subprocess.run(
        ["svgbob_cli"],
        input=src, capture_output=True, text=True
    )
    if result.returncode != 0:
        sys.stderr.write(f"svgbob error: {result.stderr}\n")
        return match.group(0)
    svg = result.stdout

    # Strip the embedded <style> block — we provide our own via CSS
    svg = re.sub(r"<style>.*?</style>", "", svg, flags=re.DOTALL)

    # Wrap in a div with our class for theme-aware styling
    # Add max-width; svgbob already adds class="svgbob"
    svg = svg.replace("<svg ", "<svg style=\"max-width:100%\" ")
    return "<div class=\"svgbob-wrap\">" + svg + "</div>"

pattern = re.compile(r"```svgbob\n(.*?)```", re.DOTALL)

def walk(obj):
    if isinstance(obj, dict):
        if "content" in obj and isinstance(obj["content"], str):
            obj["content"] = pattern.sub(replace_svgbob, obj["content"])
        for v in obj.values():
            walk(v)
    elif isinstance(obj, list):
        for item in obj:
            walk(item)

walk(book)
json.dump(book, sys.stdout)
'
