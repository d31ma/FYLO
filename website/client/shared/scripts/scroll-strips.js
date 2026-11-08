const SELECTOR = '.doc-nav-inner, .doc-sample-langs'

export function scrollState(left, width, total) {
    const max = Math.max(0, total - width)
    return { overflow: max > 1, start: left <= 1, end: left >= max - 1 }
}

export function installScrollStrips() {
    const observed = new Set()
    let drag = null
    let suppressClick = null

    const update = (strip) => {
        const wrapper = strip.closest('.scroll-strip')
        if (!wrapper) return
        const state = scrollState(strip.scrollLeft, strip.clientWidth, strip.scrollWidth)
        wrapper.classList.toggle('has-overflow', state.overflow)
        wrapper.querySelector('[data-scroll-direction="-1"]').disabled = state.start
        wrapper.querySelector('[data-scroll-direction="1"]').disabled = state.end
    }
    const resize = new ResizeObserver((entries) => entries.forEach(({ target }) => update(target)))
    const sync = () => {
        for (const strip of observed) {
            if (!strip.isConnected) {
                resize.unobserve(strip)
                observed.delete(strip)
            }
        }
        for (const strip of document.querySelectorAll(SELECTOR)) {
            if (!observed.has(strip)) {
                observed.add(strip)
                resize.observe(strip)
            }
            update(strip)
        }
    }
    document.addEventListener(
        'scroll',
        (event) => {
            if (event.target instanceof Element && event.target.matches(SELECTOR))
                update(event.target)
        },
        true
    )
    document.addEventListener(
        'click',
        (event) => {
            if (event.detail > 0 && suppressClick?.contains(event.target)) {
                suppressClick = null
                event.preventDefault()
                event.stopImmediatePropagation()
                return
            }
            const button = event.target.closest('[data-scroll-direction]')
            if (!button) return
            const strip = button.closest('.scroll-strip').querySelector(SELECTOR)
            strip.scrollBy({
                left: Number(button.dataset.scrollDirection) * strip.clientWidth * 0.8
            })
        },
        true
    )

    // Touch/trackpad scrolling stays native. Mouse dragging needs explicit support.
    document.addEventListener('pointerdown', (event) => {
        suppressClick = null
        if (event.pointerType !== 'mouse' || event.button !== 0) return
        const strip = event.target.closest(SELECTOR)
        if (!strip || strip.scrollWidth <= strip.clientWidth) return
        drag = {
            strip,
            id: event.pointerId,
            x: event.clientX,
            left: strip.scrollLeft,
            moved: false
        }
    })
    document.addEventListener('dragstart', (event) => {
        if (drag) event.preventDefault()
    })
    document.addEventListener('pointermove', (event) => {
        if (!drag || drag.id !== event.pointerId) return
        const delta = event.clientX - drag.x
        if (!drag.moved && Math.abs(delta) < 5) return
        if (!drag.moved) {
            drag.moved = true
            drag.strip.setPointerCapture(event.pointerId)
            drag.strip.classList.add('is-dragging')
        }
        event.preventDefault()
        drag.strip.scrollLeft = drag.left - delta
    })
    const endDrag = (event) => {
        if (!drag || drag.id !== event.pointerId) return
        if (drag.moved && event.type === 'pointerup') suppressClick = drag.strip
        drag.strip.classList.remove('is-dragging')
        if (drag.strip.hasPointerCapture(event.pointerId))
            drag.strip.releasePointerCapture(event.pointerId)
        drag = null
    }
    document.addEventListener('pointerup', endDrag)
    document.addEventListener('pointercancel', endDrag)
    sync()
    return sync
}
