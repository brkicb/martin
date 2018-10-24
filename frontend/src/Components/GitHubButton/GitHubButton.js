import React from 'react';
import octocat from './octocat.svg';
import Container from './Container';

const GitHubButton = () => (
  <Container href='https://github.com/urbica/martin'>
    <span>View on Github</span>
    <img src={octocat} alt='octocat' />
  </Container>
);

export default GitHubButton;
