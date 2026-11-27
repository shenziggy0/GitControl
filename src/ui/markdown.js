import MarkdownIt from 'markdown-it';

const parser = new MarkdownIt({ html:false, breaks:true }).disable('image');

const validateLink = parser.validateLink;
parser.validateLink = url => /^https?:\/\//i.test(url) && validateLink(url);
parser.renderer.rules.link_open = (tokens, index, options, _env, renderer) => {
  tokens[index].attrSet('target', '_blank');
  tokens[index].attrSet('rel', 'noopener noreferrer');
  return renderer.renderToken(tokens, index, options);
};

export function renderAgentMarkdown(text) {
  return parser.render(text);
}
