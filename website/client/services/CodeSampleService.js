import { sampleCode } from '/shared/scripts/samples.js'

/** @import LanguageService from '/services/LanguageService.js' */
@Service
@NeedsAll
export default class CodeSampleService {
    /** @type {LanguageService} */
    language

    /** @type {string} */
    topic

    /** @returns {string} */
    code() {
        return sampleCode(this.language.chosen, this.topic)
    }
}
