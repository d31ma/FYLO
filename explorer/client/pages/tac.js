const TITLE = 'FX | Fylo Explorer'

document.title = TITLE

export default class extends Tac {
    constructor(props = {}, tac = undefined) {
        super(props, tac)
        if (this.isBrowser) document.title = TITLE
    }
}
