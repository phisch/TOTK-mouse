#!/bin/bash
set -e

OUT_NSO=${OUT}/${BINARY_NAME}

# Clear older build.
rm -rf ${OUT}

# Create out directory.
mkdir -p ${OUT}

# Copy build into out
mv ${NAME}.nso ${OUT_NSO}
# Not shipped: it would replace the game's own main.npdm.
rm -f ${NAME}.npdm

# Copy ELF to user path if defined.
if [ ! -z $ELF_EXTRACT ]; then
    cp "$NAME.elf" "$ELF_EXTRACT"
fi
