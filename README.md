# There Can Only Be One (tcobo)

This is a program which finds duplicated files in a specified path.

Usage: tcobo <path>

## Optimizations

Duplicate checking happens in multiple passes. First, all files in the directory are bucketed by file size, then a small amount of the file (based on the disk's block size) is samplef from the start of the file and hashed using the [blak3](https://github.com/BLAKE3-team/BLAKE3) hashing algorithm. This quickly and inexpensively identifies true negatives that can be ruled out as duplicates.

From there, any hash collisions are inspected by fully hashing all colliding files. Any files whose hashes collide are reported as a group as duplicates.

## TODOs

1. Add a dry run mode
2. Add the ability to skip the initial hashing pass and proceed directly to hashing all files (which will probably not be useful if we expect there will be many duplicates)
3. Add chunk_size support for non-unix systems
