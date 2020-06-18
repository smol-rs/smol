#!/usr/bin/env ruby
# frozen_string_literal: true

# Commits before the bindgen-bindings/ directory was introduced can be ignored.
min = `git log --reverse --format=%H bindgen-bindings 2>&1`.split("\n").first
shas = `git log -10 --format=%H`.split("\n")
invalid = []

shas.each do |sha|
  break if sha == min

  stat = `git show --stat --format="" #{sha}`

  next unless stat.include?('wepoll/')
  next if stat.include?('bindgen-bindings')

  invalid << sha
end

unless invalid.empty?
  puts(
    'The following commit(s) change one or more files in the wepoll/ ' \
      "directory, without updating the bindgen bindings:\n\n"
  )

  invalid.each do |sha|
    puts("\e[36m#{sha}\e[0m")
  end

  puts
  puts(
    "Run `\e[33mcargo build --features buildtime-bindgen\e[0m` to " \
      'update the bindings, then include the changes in the commit ' \
      'that changes the wepoll source code.'
  )

  exit(1)
end
