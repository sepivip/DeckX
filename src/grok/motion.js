
// The discovered bindings supply the installed motion and SVG functions.
let deckxInput;
let deckxMarks = [];
let deckxPalette;

function deckxInk(color) {
    const { compact, colors, dark, palette } = deckxBindings;
    if (compact && (color === 'black' || color === 'gray')) {
        return colors[color].dark;
    }
    deckxPalette ??= palette(dark);
    const accent = color === 'violet' ? 'purple' : color;
    return deckxPalette['--sand-bg-' + accent + '-bright'] ?? colors[color].dark;
}

function deckxMark(bot) {
    const { resolve, body, motion: createMotion, state: createState, seed, appearance } = deckxBindings;
    const { shape, color } = resolve({ ...bot, agentId: bot.id });
    const motion = createMotion({ body: body(shape) });
    const state = createState(motion, 'idle', { seed: seed(bot.id), isCompact: true });
    return { motion, state, appearance: appearance(deckxInk(color)) };
}

export function deckxRender(input, elapsed, size) {
    if (input !== deckxInput) {
        const previous = new Map(deckxMarks.map(mark => [mark.key, mark]));
        deckxMarks = JSON.parse(input).map(bot => {
            const key = JSON.stringify([bot.id, bot.avatarShape, bot.avatarColor]);
            return previous.get(key) ?? { key, ...deckxMark(bot) };
        });
        deckxInput = input;
    }

    return deckxMarks.map(({ motion, state, appearance }, index) => {
        const step = Math.min(elapsed, 0.2);
        state.advance(step);
        motion.advance(step);
        const displaySize = 42;
        const { svg, serialize, compact } = deckxBindings;
        const geometrySize = compact ? Math.min(size, displaySize) : size;
        return serialize(svg(motion.frame(), appearance, geometrySize, 'avatar-' + index, {
            displaySize,
            extent: size,
        }));
    });
}
