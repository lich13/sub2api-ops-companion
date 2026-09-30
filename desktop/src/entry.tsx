import React from 'react'
import {createRoot} from 'react-dom/client'
import App from './main'
const rootElement = document.getElementById('root')!

// A second evaluation can happen during a WebView reload/HMR cycle. Keep one
// React root per document so an old surface cannot remain mounted beside it.
if (!rootElement.dataset.sub2opsMounted) {
  rootElement.dataset.sub2opsMounted = 'true'
  createRoot(rootElement).render(<React.StrictMode><App/></React.StrictMode>)
}
